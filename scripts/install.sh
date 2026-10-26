#!/bin/sh
# Mudcrab Installer for Linux & macOS
# Usage: curl -fsSL https://mudcrab.dev/install.sh | bash
#
# "Hey you, you're finally awake."

set -eu

REPO="Mudcrab-Team/mudcrab"
INSTALL_DIR="${MUDCRAB_INSTALL_DIR:-$HOME/.local/share/mudcrab}"
BIN_DIR="${MUDCRAB_BIN_DIR:-$HOME/.local/bin}"

# Colors
if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    BLUE='\033[0;34m'
    YELLOW='\033[1;33m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED=''
    GREEN=''
    BLUE=''
    YELLOW=''
    BOLD=''
    RESET=''
fi

info() {
    printf "${BLUE}${BOLD}==>${RESET} ${BOLD}%s${RESET}\n" "$1"
}

success() {
    printf "${GREEN}${BOLD}✓${RESET} %s\n" "$1"
}

warn() {
    printf "${YELLOW}${BOLD}!${RESET} %s\n" "$1"
}

error() {
    printf "${RED}${BOLD}error:${RESET} %s\n" "$1" >&2
    exit 1
}

# 1. Detect OS & Architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)
        OS_TYPE="unknown-linux-gnu"
        ;;
    Darwin)
        OS_TYPE="apple-darwin"
        ;;
    *)
        error "Unsupported operating system: $OS. Mudcrab supports Linux and macOS (Windows uses installer wizard)."
        ;;
esac

case "$ARCH" in
    x86_64|amd64)
        ARCH_TYPE="x86_64"
        ;;
    aarch64|arm64)
        if [ "$OS" = "Linux" ]; then
            error "Linux ARM64 is not yet officially distributed in pre-built releases. Please compile from source."
        fi
        ARCH_TYPE="aarch64"
        ;;
    *)
        error "Unsupported architecture: $ARCH"
        ;;
esac

TARGET="${ARCH_TYPE}-${OS_TYPE}"

info "Detected platform: ${BOLD}${TARGET}${RESET}"

# 2. Resolve Version
VERSION="${MUDCRAB_VERSION:-latest}"

if [ "$VERSION" = "latest" ]; then
    info "Resolving latest Mudcrab release..."
    LATEST_URL="$(curl -sIL -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest" 2>/dev/null || true)"
    TAG="${LATEST_URL##*/}"

    if [ -z "$TAG" ] || [ "$TAG" = "latest" ]; then
        # Fallback if no releases tagged yet
        TAG="v0.1.0"
        warn "No published releases found yet on ${REPO}. Defaulting to ${TAG}."
    fi
else
    # Ensure tag starts with 'v'
    case "$VERSION" in
        v*) TAG="$VERSION" ;;
        *)  TAG="v$VERSION" ;;
    esac
fi

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/mudcrab-${TAG}-${TARGET}.tar.gz"

info "Downloading Mudcrab ${BOLD}${TAG}${RESET} (${TARGET})..."

# 3. Create temporary directory for download
TMP_DIR="$(mktemp -d)"
cleanup() {
    rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

ARCHIVE="${TMP_DIR}/mudcrab.tar.gz"

if ! curl -fL --progress-bar "$DOWNLOAD_URL" -o "$ARCHIVE"; then
    error "Failed to download Mudcrab from: $DOWNLOAD_URL\nCheck that the release tag '${TAG}' and target '${TARGET}' exist."
fi

# 4. Extract into installation directory
info "Installing to ${BOLD}${INSTALL_DIR}${RESET}..."

mkdir -p "${INSTALL_DIR}/bin" "${BIN_DIR}"
tar -xzf "$ARCHIVE" -C "${INSTALL_DIR}/bin"

# 5. Symlink binaries to BIN_DIR
BINARIES="launcher engine converter world-inspect"
for bin in $BINARIES; do
    if [ -f "${INSTALL_DIR}/bin/${bin}" ]; then
        chmod +x "${INSTALL_DIR}/bin/${bin}"
        ln -sf "${INSTALL_DIR}/bin/${bin}" "${BIN_DIR}/${bin}"
    fi
done

# 6. Linux Desktop integration
if [ "$OS" = "Linux" ]; then
    APPS_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
    mkdir -p "$APPS_DIR"
    cat <<EOF > "${APPS_DIR}/mudcrab.desktop"
[Desktop Entry]
Name=Mudcrab
Comment=Modern Skyrim Engine and Converter
Exec=${BIN_DIR}/launcher
Terminal=false
Type=Application
Categories=Game;
StartupNotify=true
EOF
    success "Created desktop entry: ${APPS_DIR}/mudcrab.desktop"
fi

success "Installed Mudcrab binaries to ${BOLD}${BIN_DIR}${RESET}"

# 7. Check PATH
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        printf "\n"
        warn "${BIN_DIR} is not in your PATH!"
        printf "To run 'launcher', 'engine', or 'converter' from anywhere, add this to your shell profile:\n\n"
        printf "  ${BOLD}export PATH=\"\$HOME/.local/bin:\$PATH\"${RESET}\n\n"
        ;;
esac

printf "\n"
printf "${GREEN}${BOLD}🦀 Mudcrab ${TAG} is ready!${RESET}\n"
printf "Run ${BOLD}launcher${RESET} to configure your Skyrim data and launch the engine.\n"
