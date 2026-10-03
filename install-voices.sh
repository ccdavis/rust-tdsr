#!/bin/bash
#
# Install the speech engines and voices TDSR's ALSA backend can use (Linux):
# espeak-ng, MBROLA, RHVoice and SVOX Pico from the distribution's packages,
# and Piper neural voices downloaded into your home directory.
#

set -e

PIPER_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/piper-voices"
PIPER_URL="https://huggingface.co/rhasspy/piper-voices/resolve/main"

# Piper voices fetched by default: the high-quality English ones, then some
# medium ones, which answer faster. All voices are listed at
# https://huggingface.co/rhasspy/piper-voices
PIPER_VOICES=(
    en_US-ryan-high
    en_US-lessac-high
    en_US-ljspeech-high
    en_GB-cori-high
    en_US-joe-medium
    en_US-amy-medium
    en_US-hfc_female-medium
    en_US-hfc_male-medium
    en_GB-alan-medium
)

PACKAGES=1
PIPER=1
CUSTOM_VOICES=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --no-packages) PACKAGES=0 ;;
        --no-piper) PIPER=0 ;;
        --piper-dir)
            PIPER_DIR="$2"
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [options] [piper-voice ...]"
            echo "  --no-packages    Skip the system packages (needs no root)"
            echo "  --no-piper       Skip downloading Piper voices"
            echo "  --piper-dir DIR  Where Piper voices go (default: $PIPER_DIR)"
            echo "  piper-voice      Voices to download instead of the default set,"
            echo "                   e.g. en_US-ryan-high de_DE-thorsten-high"
            echo
            echo "Default Piper voices: ${PIPER_VOICES[*]}"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1 (see --help)" >&2
            exit 1
            ;;
        *) CUSTOM_VOICES+=("$1") ;;
    esac
    shift
done
if [[ ${#CUSTOM_VOICES[@]} -gt 0 ]]; then
    PIPER_VOICES=("${CUSTOM_VOICES[@]}")
fi

# ---- System packages --------------------------------------------------------

# The first of the given package names that apt knows how to install
# (library package names differ between releases: libttspico0 / libttspico0t64).
first_available() {
    local pkg
    for pkg in "$@"; do
        if apt-cache policy "$pkg" 2>/dev/null | grep -q 'Candidate: [^(]'; then
            echo "$pkg"
            return
        fi
    done
}

install_packages() {
    if ! command -v apt-get &> /dev/null; then
        echo "No apt-get here: install these with your package manager instead:"
        echo "  espeak-ng (with libespeak-ng), alsa-lib, mbrola and its voices,"
        echo "  RHVoice with an English voice, SVOX Pico (libttspico, picotts)"
        return
    fi

    local sudo=()
    if [[ $EUID -ne 0 ]]; then
        sudo=(sudo)
    fi

    echo "Updating the package lists..."
    "${sudo[@]}" apt-get update

    local wanted=(
        "espeak-ng"
        "libespeak-ng1"
        "libasound2t64 libasound2"
        "mbrola"
        "mbrola-us1"
        "mbrola-us2"
        "mbrola-us3"
        "mbrola-en1"
        "librhvoice5"
        "rhvoice-english"
        "libttspico0t64 libttspico0"
        "libttspico-data"
    )
    local install=() missing=() names pkg
    for names in "${wanted[@]}"; do
        # shellcheck disable=SC2086
        pkg=$(first_available $names)
        if [[ -n "$pkg" ]]; then
            install+=("$pkg")
        else
            missing+=("${names%% *}")
        fi
    done

    echo "Installing: ${install[*]}"
    "${sudo[@]}" apt-get install -y "${install[@]}"

    if [[ ${#missing[@]} -gt 0 ]]; then
        echo
        echo "Not offered by your package sources: ${missing[*]}"
        echo "MBROLA voices, RHVoice and Pico live in Ubuntu's 'multiverse' and"
        echo "Debian's 'contrib'/'non-free' components; enable them and run this again."
    fi
}

# ---- Piper voices -----------------------------------------------------------

fetch() {
    if command -v curl &> /dev/null; then
        curl -fL --progress-bar -o "$2" "$1"
    else
        wget -q --show-progress -O "$2" "$1"
    fi
}

# A voice LOCALE-NAME-QUALITY (en_US-ryan-high) is the two files
# LANG/LOCALE/NAME/QUALITY/LOCALE-NAME-QUALITY.onnx and .onnx.json
install_piper_voices() {
    if ! command -v curl &> /dev/null && ! command -v wget &> /dev/null; then
        echo "Piper voices need curl or wget to download" >&2
        return 1
    fi
    mkdir -p "$PIPER_DIR"
    local voice locale rest name quality base file failed=()
    for voice in "${PIPER_VOICES[@]}"; do
        locale=${voice%%-*}
        rest=${voice#*-}
        name=${rest%-*}
        quality=${rest##*-}
        if [[ "$rest" == "$voice" || "$name" == "$rest" ]]; then
            echo "Not a Piper voice name (want e.g. en_US-ryan-high): $voice" >&2
            failed+=("$voice")
            continue
        fi
        base="$PIPER_URL/${locale%%_*}/$locale/$name/$quality"
        if [[ -s "$PIPER_DIR/$voice.onnx" && -s "$PIPER_DIR/$voice.onnx.json" ]]; then
            echo "Piper voice $voice: already there"
            continue
        fi
        echo "Piper voice $voice:"
        for file in "$voice.onnx.json" "$voice.onnx"; do
            # Download beside the target and rename, so that an interrupted
            # download never leaves half a voice for TDSR to find
            if fetch "$base/$file" "$PIPER_DIR/$file.part"; then
                mv "$PIPER_DIR/$file.part" "$PIPER_DIR/$file"
            else
                rm -f "$PIPER_DIR/$file.part"
                failed+=("$voice")
                break
            fi
        done
    done
    echo "Piper voices in $PIPER_DIR:"
    ls "$PIPER_DIR" | sed -n 's/\.onnx$//p' | sed 's/^/  /'
    if [[ ${#failed[@]} -gt 0 ]]; then
        echo "Could not download: ${failed[*]}" >&2
        return 1
    fi
}

if [[ $PACKAGES -eq 1 ]]; then
    install_packages
    echo
fi
if [[ $PIPER -eq 1 ]]; then
    install_piper_voices
    echo
fi

echo "Done. To use these voices, build TDSR with Piper support:"
echo "  cargo build --release --features piper"
echo "and put this in ~/.tdsr.cfg:"
echo "  [speech]"
echo "  backend = alsa"
echo "Then Alt+s steps through the engines."
