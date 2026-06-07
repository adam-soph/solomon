#!/bin/sh
# install.sh — download and install the `hcc` HolyC compiler (solomon) into your PATH.
#
# solomon ships a single, self-contained binary: `hcc`. The standard library is
# embedded at build time, so there is nothing else to install. This script detects
# your OS/arch, downloads the matching prebuilt binary from the GitHub release, and
# drops it into a directory on your PATH.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/adam-soph/solomon/main/install.sh | sh
#   ./install.sh                       # install the latest release
#   ./install.sh --version v0.1.0      # install a specific tag
#   ./install.sh --dir /usr/local/bin  # choose the install directory
#
# Environment overrides (handy when piping through `sh`):
#   HCC_VERSION       release tag to install (default: latest)
#   HCC_INSTALL_DIR   directory to install into (default: see "pick_install_dir")
#
# Supported hosts: Linux (x86_64, aarch64), macOS (Apple silicon + Intel, via the
# universal binary), and Windows (x86_64, i686) when run under a POSIX shell such as
# Git Bash or MSYS2.

set -eu

REPO="adam-soph/solomon"
BIN="hcc"

# --- pretty output ----------------------------------------------------------------

# Colours only when stderr is a terminal.
if [ -t 2 ]; then
	BOLD="$(printf '\033[1m')"; RED="$(printf '\033[31m')"
	GREEN="$(printf '\033[32m')"; YELLOW="$(printf '\033[33m')"; RESET="$(printf '\033[0m')"
else
	BOLD=""; RED=""; GREEN=""; YELLOW=""; RESET=""
fi

info()  { printf '%s\n' "${BOLD}==>${RESET} $*" >&2; }
warn()  { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
ok()    { printf '%s\n' "${GREEN}$*${RESET}" >&2; }
die()   { printf '%s\n' "${RED}error:${RESET} $*" >&2; exit 1; }

usage() {
	# Print the contiguous comment block after the shebang (stops at the first
	# non-comment line), stripping the leading "# ".
	awk 'NR==1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
	exit "${1:-0}"
}

# --- argument parsing -------------------------------------------------------------

VERSION="${HCC_VERSION:-latest}"
INSTALL_DIR="${HCC_INSTALL_DIR:-}"

while [ $# -gt 0 ]; do
	case "$1" in
		--version|-v) VERSION="${2:?--version needs an argument}"; shift 2 ;;
		--version=*)  VERSION="${1#*=}"; shift ;;
		--dir|-d)     INSTALL_DIR="${2:?--dir needs an argument}"; shift 2 ;;
		--dir=*)      INSTALL_DIR="${1#*=}"; shift ;;
		--help|-h)    usage 0 ;;
		*)            die "unknown argument: $1 (try --help)" ;;
	esac
done

# --- host detection ---------------------------------------------------------------

# Choose a downloader once, up front.
if command -v curl >/dev/null 2>&1; then
	DL="curl"
elif command -v wget >/dev/null 2>&1; then
	DL="wget"
else
	die "need either curl or wget installed to download the binary"
fi

detect_os() {
	case "$(uname -s)" in
		Linux)                       echo linux ;;
		Darwin)                      echo macos ;;
		MINGW*|MSYS*|CYGWIN*|Windows*) echo windows ;;
		*) die "unsupported OS: $(uname -s)" ;;
	esac
}

detect_arch() {
	case "$(uname -m)" in
		x86_64|amd64)   echo x86_64 ;;
		aarch64|arm64)  echo aarch64 ;;
		i686|i386)      echo i686 ;;
		*) die "unsupported architecture: $(uname -m)" ;;
	esac
}

OS="$(detect_os)"
ARCH="$(detect_arch)"
EXT=""

# Map (OS, arch) -> the release asset name produced by .github/workflows/release.yml.
# macOS uses the universal (fat) binary so a single asset covers both arches.
# Linux/x86_64 prefers the static musl build (no glibc version dependency).
case "$OS" in
	macos)
		ASSET="${BIN}-macos-universal"
		;;
	linux)
		case "$ARCH" in
			x86_64)  ASSET="${BIN}-x86_64-unknown-linux-musl" ;;
			aarch64) ASSET="${BIN}-aarch64-unknown-linux-gnu" ;;
			*) die "no Linux release binary for $ARCH" ;;
		esac
		;;
	windows)
		EXT=".exe"
		case "$ARCH" in
			x86_64) ASSET="${BIN}-x86_64-pc-windows-msvc.exe" ;;
			i686)   ASSET="${BIN}-i686-pc-windows-msvc.exe" ;;
			*) die "no Windows release binary for $ARCH" ;;
		esac
		;;
esac

# --- where to install -------------------------------------------------------------

# Default: a no-sudo location. Prefer /usr/local/bin only when it is writable
# without elevation; otherwise fall back to ~/.local/bin (created if needed).
pick_install_dir() {
	if [ -n "$INSTALL_DIR" ]; then
		echo "$INSTALL_DIR"; return
	fi
	if [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
		echo /usr/local/bin; return
	fi
	echo "${HOME}/.local/bin"
}

INSTALL_DIR="$(pick_install_dir)"
DEST="${INSTALL_DIR}/${BIN}${EXT}"

# --- download URL -----------------------------------------------------------------

if [ "$VERSION" = "latest" ]; then
	URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
	URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

# --- do it ------------------------------------------------------------------------

download() {
	# download <url> <dest>
	if [ "$DL" = "curl" ]; then
		# -f: fail (non-zero) on HTTP errors instead of saving the 404 page.
		curl -fSL --progress-bar "$1" -o "$2"
	else
		wget -q --show-progress -O "$2" "$1"
	fi
}

info "installing ${BOLD}${BIN}${RESET} (${VERSION}) for ${OS}/${ARCH}"
info "asset:   ${ASSET}"
info "from:    ${URL}"
info "into:    ${DEST}"

mkdir -p "$INSTALL_DIR" || die "could not create install directory: $INSTALL_DIR"

# Download to a temp file first so a failed/partial download never clobbers an
# existing install.
TMP="$(mktemp "${TMPDIR:-/tmp}/hcc-install.XXXXXX")"
trap 'rm -f "$TMP"' EXIT INT TERM

if ! download "$URL" "$TMP"; then
	die "download failed. Check that release '${VERSION}' exists and has asset '${ASSET}'."
fi

[ -s "$TMP" ] || die "downloaded file is empty — release asset may be missing"

chmod +x "$TMP"

# Move into place; retry through sudo if the destination needs elevation.
if mv "$TMP" "$DEST" 2>/dev/null; then
	:
elif command -v sudo >/dev/null 2>&1; then
	warn "no write permission for ${INSTALL_DIR}; retrying with sudo"
	sudo mv "$TMP" "$DEST" || die "failed to install to ${DEST}"
else
	die "no write permission for ${INSTALL_DIR} and sudo is unavailable. Re-run with --dir <writable dir>."
fi
trap - EXIT INT TERM

ok "installed ${BIN} -> ${DEST}"

# --- PATH advice ------------------------------------------------------------------

# Is the install dir already on PATH?
case ":${PATH}:" in
	*":${INSTALL_DIR}:"*) ON_PATH=1 ;;
	*) ON_PATH=0 ;;
esac

if [ "$ON_PATH" -eq 1 ]; then
	info "run it: ${BOLD}${BIN} --help${RESET}"
else
	warn "${INSTALL_DIR} is not on your PATH."
	printf '\n  Add it by appending this to your shell profile (~/.bashrc, ~/.zshrc, ...):\n\n' >&2
	printf '    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
	info "then run: ${BOLD}${BIN} --help${RESET}  (or use the full path: ${DEST})"
fi
