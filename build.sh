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

# Check Linux build dependencies
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    echo -e "${YELLOW}Checking build dependencies...${NC}"
    MISSING=0

    # Check for libclang (needed by bindgen for tts crate)
    # libclang doesn't use pkg-config, check for the library directly
    if ls /usr/lib/llvm-*/lib/libclang.so* &>/dev/null || \
       ls /usr/lib/x86_64-linux-gnu/libclang*.so* &>/dev/null || \
       ls /usr/lib/libclang*.so* &>/dev/null; then
        echo -e "${GREEN}✓${NC} libclang found"
    else
        echo -e "${RED}✗${NC} libclang-dev not found"
        echo "  Install: sudo apt install libclang-dev"
        MISSING=1
    fi

    # Check for speech-dispatcher dev headers
    if pkg-config --exists speech-dispatcher 2>/dev/null; then
        echo -e "${GREEN}✓${NC} libspeechd-dev found"
    else
        echo -e "${RED}✗${NC} libspeechd-dev not found"
        echo "  Install: sudo apt install libspeechd-dev"
        MISSING=1
    fi

    if [[ $MISSING -eq 1 ]]; then
        echo
        echo -e "${RED}Missing dependencies. Install them and retry.${NC}"
        exit 1
    fi
    echo
fi

# Handle arguments
SKIP_TESTS=0
CLEAN=0
for arg in "$@"; do
    case $arg in
        --clean) CLEAN=1 ;;
        --no-test) SKIP_TESTS=1 ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo "  --clean    Clean before building"
            echo "  --no-test  Skip running tests"
            exit 0
            ;;
    esac
done

# Clean if requested
if [[ $CLEAN -eq 1 ]]; then
    echo -e "${YELLOW}Cleaning...${NC}"
    cargo clean
    echo
fi

# Run tests
if [[ $SKIP_TESTS -eq 0 ]]; then
    echo -e "${BLUE}Running tests...${NC}"
    if cargo test --quiet 2>/dev/null; then
        echo -e "${GREEN}✓${NC} Tests passed"
    else
        echo -e "${RED}✗${NC} Tests failed"
        exit 1
    fi
    echo
fi

# Build
echo -e "${BLUE}Building release...${NC}"
cargo build --release

BINARY="target/release/tdsr"
if [[ -f "$BINARY" ]]; then
    echo
    SIZE=$(du -h "$BINARY" | cut -f1)
    echo -e "${GREEN}✓${NC} Build complete: $BINARY ($SIZE)"
    echo
    echo "Install with:"
    echo "  cargo install --path ."
    echo "  # or"
    echo "  cp $BINARY ~/.local/bin/"
else
    echo -e "${RED}Error: Binary not found${NC}"
    exit 1
fi
