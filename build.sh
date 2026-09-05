#!/bin/bash
#
# Build script for TDSR Rust edition
#

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}TDSR Build Script${NC}"
echo

# Check Rust
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error: Rust/Cargo not found${NC}"
    echo "Install from: https://rustup.rs/"
    exit 1
fi

echo -e "${GREEN}✓${NC} Rust: $(rustc --version | cut -d' ' -f2)"
echo -e "${GREEN}✓${NC} Cargo: $(cargo --version | cut -d' ' -f2)"
echo

# Handle arguments
SKIP_TESTS=0
CLEAN=0
ESPEAK_ONLY=0
for arg in "$@"; do
    case $arg in
        --clean) CLEAN=1 ;;
        --no-test) SKIP_TESTS=1 ;;
        --espeak-only) ESPEAK_ONLY=1 ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo "  --clean        Clean before building"
            echo "  --no-test      Skip running tests"
            echo "  --espeak-only  Build without Speech Dispatcher (PulseAudio + espeak-ng);"
            echo "                 use on distros whose speech-dispatcher is too old (< 0.10),"
            echo "                 e.g. Ubuntu 20.04. Skips the libclang/libspeechd build deps."
            exit 0
            ;;
    esac
done

# CARGO_FLAGS carries feature selection into both `cargo test` and `cargo build`.
CARGO_FLAGS=()

# Check Linux build dependencies and choose a build mode.
#
# Full build (default): includes the Speech Dispatcher backend. Needs
#   libclang-dev and libspeechd-dev >= 0.10 at build time.
# espeak-ng-only build (--no-default-features): no Speech Dispatcher, no
#   libclang/libspeechd needed. Auto-selected when speech-dispatcher is missing
#   or too old, or forced with --espeak-only.
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    echo -e "${YELLOW}Checking build dependencies...${NC}"

    # Is the system speech-dispatcher new enough for the `speech-dispatcher` crate (>= 0.10)?
    SPEECHD_OK=0
    SPD_VER="$(pkg-config --modversion speech-dispatcher 2>/dev/null || true)"
    if [[ -n "$SPD_VER" ]]; then
        SPD_MAJ="${SPD_VER%%.*}"; SPD_REST="${SPD_VER#*.}"; SPD_MIN="${SPD_REST%%.*}"
        if [[ "$SPD_MAJ" -gt 0 || "$SPD_MIN" -ge 10 ]]; then
            SPEECHD_OK=1
        fi
    fi

    # Does libclang exist (bindgen needs it for the Speech Dispatcher bindings)?
    LIBCLANG_OK=0
    if ls /usr/lib/llvm-*/lib/libclang.so* &>/dev/null || \
       ls /usr/lib/x86_64-linux-gnu/libclang*.so* &>/dev/null || \
       ls /usr/lib/libclang*.so* &>/dev/null; then
        LIBCLANG_OK=1
    fi

    if [[ $ESPEAK_ONLY -eq 1 || $SPEECHD_OK -eq 0 || $LIBCLANG_OK -eq 0 ]]; then
        CARGO_FLAGS+=(--no-default-features)
        if [[ $ESPEAK_ONLY -eq 1 ]]; then
            echo -e "${BLUE}ℹ${NC} Building espeak-ng-only variant (--espeak-only)."
        elif [[ -n "$SPD_VER" && $SPEECHD_OK -eq 0 ]]; then
            echo -e "${YELLOW}ℹ${NC} speech-dispatcher $SPD_VER is too old (< 0.10) for the Speech"
            echo -e "   Dispatcher backend; building espeak-ng-only variant."
        else
            echo -e "${YELLOW}ℹ${NC} Speech Dispatcher build deps not found; building espeak-ng-only variant."
        fi
        if ! command -v espeak-ng &>/dev/null; then
            echo -e "${RED}✗${NC} espeak-ng not found (required for this build)."
            echo "  Install with: ./install-deps.sh   (or: sudo apt install espeak-ng)"
            exit 1
        fi
        echo -e "${GREEN}✓${NC} espeak-ng found"
    else
        echo -e "${GREEN}✓${NC} libclang found"
        echo -e "${GREEN}✓${NC} speech-dispatcher $SPD_VER (Speech Dispatcher backend)"
    fi
    echo
fi

# Clean if requested
if [[ $CLEAN -eq 1 ]]; then
    echo -e "${YELLOW}Cleaning...${NC}"
    cargo clean
    echo
fi

# Run tests
if [[ $SKIP_TESTS -eq 0 ]]; then
    echo -e "${BLUE}Running tests...${NC}"
    if cargo test --quiet "${CARGO_FLAGS[@]}" 2>/dev/null; then
        echo -e "${GREEN}✓${NC} Tests passed"
    else
        echo -e "${RED}✗${NC} Tests failed"
        exit 1
    fi
    echo
fi

# Build
echo -e "${BLUE}Building release...${NC}"
cargo build --release "${CARGO_FLAGS[@]}"

BINARY="target/release/tdsr"
if [[ -f "$BINARY" ]]; then
    echo
    SIZE=$(du -h "$BINARY" | cut -f1)
    echo -e "${GREEN}✓${NC} Build complete: $BINARY ($SIZE)"
    echo
    echo "Install with:"
    echo "  cargo install --path . ${CARGO_FLAGS[*]}"
    echo "  # or"
    echo "  cp $BINARY ~/.local/bin/"
else
    echo -e "${RED}Error: Binary not found${NC}"
    exit 1
fi
