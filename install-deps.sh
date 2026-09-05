#!/bin/bash
#
# install-deps.sh — install TDSR build and runtime prerequisites
#
# Detects the platform (macOS, WSL, native Linux) and the Linux package
# manager (apt / dnf / pacman / zypper), then installs:
#   - build dependencies   (libclang, speech-dispatcher dev headers)
#   - runtime dependencies  (speech-dispatcher, espeak-ng, clipboard tools)
#   - optional extras       (MBROLA voices) with --with-mbrola
#
# It does NOT build TDSR — run ./build.sh (or cargo build --release) after.
#
# Usage:
#   ./install-deps.sh [--with-mbrola] [--dry-run] [--yes]
#
#   --with-mbrola   also install MBROLA voices (higher quality, Linux/WSL)
#   --dry-run       print the commands that would run, install nothing
#   --yes           pass the package manager's assume-yes flag
#

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

WITH_MBROLA=0
DRY_RUN=0
ASSUME_YES=0
for arg in "$@"; do
    case $arg in
        --with-mbrola) WITH_MBROLA=1 ;;
        --dry-run)     DRY_RUN=1 ;;
        --yes|-y)      ASSUME_YES=1 ;;
        --help|-h)
            cat <<'EOF'
install-deps.sh — install TDSR build and runtime prerequisites

Detects the platform (macOS, WSL, native Linux) and the Linux package
manager (apt / dnf / pacman / zypper), then installs build deps,
runtime deps, and (optionally) MBROLA voices. Does not build TDSR.

Usage:
  ./install-deps.sh [--with-mbrola] [--dry-run] [--yes]

  --with-mbrola   also install MBROLA voices (higher quality, Linux/WSL)
  --dry-run       print the commands that would run, install nothing
  --yes, -y       pass the package manager's assume-yes flag
EOF
            exit 0
            ;;
        *)
            echo -e "${RED}Unknown option: $arg${NC}" >&2
            exit 1
            ;;
    esac
done

echo -e "${BLUE}TDSR dependency installer${NC}"
echo

# run() prints a command and (unless --dry-run) executes it
run() {
    echo -e "${YELLOW}\$ $*${NC}"
    if [[ $DRY_RUN -eq 0 ]]; then
        "$@"
    fi
}

note_rust() {
    if ! command -v cargo &>/dev/null; then
        echo
        echo -e "${YELLOW}Note:${NC} Rust/Cargo not found. Install it with:"
        echo "  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    else
        echo -e "${GREEN}✓${NC} Rust toolchain: $(rustc --version | cut -d' ' -f2)"
    fi
}

# ---------------------------------------------------------------------------
# macOS
# ---------------------------------------------------------------------------
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo -e "${GREEN}macOS detected.${NC}"
    echo "No build or runtime packages are required — TDSR uses the built-in"
    echo "AVFoundation speech synthesizer (macOS 10.14+)."
    note_rust
    echo
    echo -e "${GREEN}Done.${NC} Next: ./build.sh"
    exit 0
fi

# ---------------------------------------------------------------------------
# Linux / WSL
# ---------------------------------------------------------------------------
IS_WSL=0
if grep -qi microsoft /proc/version 2>/dev/null; then
    IS_WSL=1
    echo -e "${GREEN}WSL detected.${NC} Runtime speech uses Windows SAPI / PulseAudio+espeak-ng."
else
    echo -e "${GREEN}Native Linux detected.${NC} Runtime speech uses Speech Dispatcher."
fi
echo

# Detect package manager
PM=""
for candidate in apt-get dnf pacman zypper; do
    if command -v "$candidate" &>/dev/null; then
        PM="$candidate"
        break
    fi
done

if [[ -z "$PM" ]]; then
    echo -e "${RED}No supported package manager found (apt/dnf/pacman/zypper).${NC}"
    echo "Install these manually:"
    echo "  build:   a libclang/clang dev package + speech-dispatcher dev headers + pkg-config"
    echo "  runtime: speech-dispatcher, espeak-ng, xclip or wl-clipboard"
    exit 1
fi

SUDO=""
if [[ $EUID -ne 0 ]]; then
    SUDO="sudo"
fi

# Per-package-manager package names and install command.
# BUILD = headers/tools needed to compile; RUNTIME = needed to run.
case "$PM" in
    apt-get)
        UPDATE=("$SUDO" apt-get update)
        INSTALL=("$SUDO" apt-get install)
        [[ $ASSUME_YES -eq 1 ]] && INSTALL+=(-y)
        BUILD=(libclang-dev libspeechd-dev pkg-config build-essential)
        # pulseaudio-utils provides parec, which the espeak-ng backend uses on
        # WSLg to wake the audio sink after a pause.
        RUNTIME=(speech-dispatcher espeak-ng pulseaudio-utils xclip wl-clipboard)
        MBROLA=(mbrola mbrola-us1 mbrola-us2)
        ;;
    dnf)
        UPDATE=(true)
        INSTALL=("$SUDO" dnf install)
        [[ $ASSUME_YES -eq 1 ]] && INSTALL+=(-y)
        BUILD=(clang-devel speech-dispatcher-devel pkgconf-pkg-config gcc)
        RUNTIME=(speech-dispatcher espeak-ng xclip wl-clipboard)
        MBROLA=(mbrola)
        ;;
    pacman)
        UPDATE=("$SUDO" pacman -Sy)
        INSTALL=("$SUDO" pacman -S --needed)
        [[ $ASSUME_YES -eq 1 ]] && INSTALL+=(--noconfirm)
        BUILD=(clang speech-dispatcher pkgconf base-devel)
        RUNTIME=(speech-dispatcher espeak-ng xclip wl-clipboard)
        # MBROLA voices live in the AUR on Arch; not installable via pacman.
        MBROLA=()
        ;;
    zypper)
        UPDATE=(true)
        INSTALL=("$SUDO" zypper install)
        [[ $ASSUME_YES -eq 1 ]] && INSTALL+=(-y)
        BUILD=(clang-devel libspeechd-devel pkg-config gcc)
        RUNTIME=(speech-dispatcher espeak-ng xclip wl-clipboard)
        MBROLA=(mbrola)
        ;;
esac

echo -e "${BLUE}Package manager:${NC} $PM"
echo -e "${BLUE}Build deps:${NC}   ${BUILD[*]}"
echo -e "${BLUE}Runtime deps:${NC} ${RUNTIME[*]}"
if [[ $WITH_MBROLA -eq 1 ]]; then
    if [[ ${#MBROLA[@]} -gt 0 ]]; then
        echo -e "${BLUE}MBROLA:${NC}       ${MBROLA[*]}"
    else
        echo -e "${YELLOW}MBROLA:${NC}       not available via $PM (see AUR / distro docs)"
    fi
fi
echo

PKGS=("${BUILD[@]}" "${RUNTIME[@]}")
if [[ $WITH_MBROLA -eq 1 && ${#MBROLA[@]} -gt 0 ]]; then
    PKGS+=("${MBROLA[@]}")
fi

run "${UPDATE[@]}"
run "${INSTALL[@]}" "${PKGS[@]}"

note_rust

# Note if the system speech-dispatcher is too old for the `speech-dispatcher`
# crate (needs >= 0.10, which introduced SPD_PUNCT_MOST and the unsigned uid
# APIs). e.g. Ubuntu 20.04 ships 0.9.1. Such systems build the espeak-ng-only
# variant instead — build.sh detects this automatically.
SPEECHD_OLD=0
if [[ $IS_WSL -eq 0 ]]; then
    SPD_VER="$(pkg-config --modversion speech-dispatcher 2>/dev/null || true)"
    if [[ -n "$SPD_VER" ]]; then
        SPD_MAJ="${SPD_VER%%.*}"
        SPD_REST="${SPD_VER#*.}"; SPD_MIN="${SPD_REST%%.*}"
        if [[ "$SPD_MAJ" -eq 0 && "$SPD_MIN" -lt 10 ]]; then
            SPEECHD_OLD=1
            echo
            echo -e "${YELLOW}Note:${NC} speech-dispatcher $SPD_VER is too old (< 0.10) for the"
            echo "Speech Dispatcher backend. TDSR will build the espeak-ng-only variant"
            echo "(low-latency PulseAudio + espeak-ng); build.sh selects this automatically."
        fi
    fi
fi

echo
echo -e "${GREEN}✓ Prerequisites installed.${NC}"
echo "Next: ./build.sh    (or: cargo build --release)"
if [[ $IS_WSL -eq 0 ]]; then
    echo
    echo "Verify speech is working:"
    if [[ $SPEECHD_OLD -eq 1 ]]; then
        echo "  espeak-ng test"
    else
        echo "  systemctl --user start speech-dispatcher && spd-say test"
        echo "  espeak-ng test   # for the PulseAudio fallback"
    fi
fi
