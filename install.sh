#!/bin/sh
# install.sh — download and install the `hcc` HolyC compiler.
#
# hcc installs Go-style into a single root directory (HCC_ROOT, default ~/.hcc): the
# compiler binary at $HCC_ROOT/bin/hcc and the standard library at $HCC_ROOT/lib. This
# script detects your OS/arch, downloads the matching prebuilt binary and the stdlib
# archive from the GitHub release, lays them out under HCC_ROOT, and adds HCC_ROOT (and
# $HCC_ROOT/bin on your PATH) to your shell profile — just like GOROOT.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/adam-soph/solomon/main/install.sh | sh
#   ./install.sh                       # install the latest release
#   ./install.sh --version v0.1.0      # install a specific tag
#   ./install.sh --root ~/sdk/hcc      # choose the install root (HCC_ROOT)
#
# Environment overrides (handy when piping through `sh`):
#   HCC_VERSION   release tag to install (default: latest)
#   HCC_ROOT      install root (default: ~/.hcc)
#
# Supported hosts: Linux (x86_64, aarch64), macOS (Apple silicon + Intel, via the
# universal binary), and Windows (x86_64, i686) when run under a POSIX shell such as
# Git Bash or MSYS2.

set -eu

REPO="adam-soph/solomon"
BIN="hcc"
STDLIB_ASSET="hcc-stdlib.tar.gz"

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
ROOT="${HCC_ROOT:-}"

while [ $# -gt 0 ]; do
	case "$1" in
		--version|-v) VERSION="${2:?--version needs an argument}"; shift 2 ;;
		--version=*)  VERSION="${1#*=}"; shift ;;
		--root|-r)    ROOT="${2:?--root needs an argument}"; shift 2 ;;
		--root=*)     ROOT="${1#*=}"; shift ;;
		--help|-h)    usage 0 ;;
		*)            die "unknown argument: $1 (try --help)" ;;
	esac
done

# Default install root, Go's GOROOT style: a single self-contained tree.
[ -n "$ROOT" ] || ROOT="${HOME}/.hcc"

# --- host detection ---------------------------------------------------------------

# Choose a downloader once, up front.
if command -v curl >/dev/null 2>&1; then
	DL="curl"
elif command -v wget >/dev/null 2>&1; then
	DL="wget"
else
	die "need either curl or wget installed to download the release"
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

# --- layout under HCC_ROOT --------------------------------------------------------

BIN_DIR="${ROOT}/bin"
LIB_DIR="${ROOT}/lib"
DEST="${BIN_DIR}/${BIN}${EXT}"

# --- download URLs ----------------------------------------------------------------

if [ "$VERSION" = "latest" ]; then
	base="https://github.com/${REPO}/releases/latest/download"
else
	base="https://github.com/${REPO}/releases/download/${VERSION}"
fi
BIN_URL="${base}/${ASSET}"
STDLIB_URL="${base}/${STDLIB_ASSET}"

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
info "binary:  ${ASSET}"
info "stdlib:  ${STDLIB_ASSET}"
info "root:    ${ROOT}"

mkdir -p "$BIN_DIR" "$LIB_DIR" || die "could not create install root: $ROOT"

# Stage downloads in a temp dir first, so a failed/partial download never clobbers an
# existing install.
TMP="$(mktemp -d "${TMPDIR:-/tmp}/hcc-install.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT INT TERM

info "downloading the compiler…"
if ! download "$BIN_URL" "${TMP}/${BIN}${EXT}"; then
	die "binary download failed. Check that release '${VERSION}' exists and has asset '${ASSET}'."
fi
[ -s "${TMP}/${BIN}${EXT}" ] || die "downloaded binary is empty — release asset may be missing"

info "downloading the standard library…"
if ! download "$STDLIB_URL" "${TMP}/${STDLIB_ASSET}"; then
	die "stdlib download failed. Check that release '${VERSION}' has asset '${STDLIB_ASSET}'."
fi
[ -s "${TMP}/${STDLIB_ASSET}" ] || die "downloaded stdlib is empty — release asset may be missing"

# Install the binary.
chmod +x "${TMP}/${BIN}${EXT}"
mv "${TMP}/${BIN}${EXT}" "$DEST" || die "failed to install the binary to ${DEST}"

# Install the standard library: extract the archive into $HCC_ROOT/lib, replacing any
# previous copy so an upgrade never leaves stale modules behind.
rm -rf "${LIB_DIR:?}"/* 2>/dev/null || true
tar -xzf "${TMP}/${STDLIB_ASSET}" -C "$LIB_DIR" || die "failed to extract the stdlib into ${LIB_DIR}"

trap - EXIT INT TERM
rm -rf "$TMP"

ok "installed ${BIN} -> ${DEST}"
ok "installed stdlib -> ${LIB_DIR}"

# --- shell profile (HCC_ROOT + PATH), GOROOT-style --------------------------------

# Pick the profile for the user's login shell, falling back to ~/.profile.
profile_for_shell() {
	case "${SHELL##*/}" in
		zsh)  echo "${ZDOTDIR:-$HOME}/.zshrc" ;;
		bash) [ -f "$HOME/.bashrc" ] && echo "$HOME/.bashrc" || echo "$HOME/.bash_profile" ;;
		fish) echo "$HOME/.config/fish/config.fish" ;;
		*)    echo "$HOME/.profile" ;;
	esac
}
PROFILE="$(profile_for_shell)"

already_on_path() {
	case ":${PATH}:" in *":${BIN_DIR}:"*) return 0 ;; *) return 1 ;; esac
}

if [ -f "$PROFILE" ] && grep -q 'HCC_ROOT' "$PROFILE" 2>/dev/null; then
	info "HCC_ROOT already configured in ${PROFILE}"
else
	mkdir -p "$(dirname "$PROFILE")"
	{
		printf '\n# added by the hcc installer\n'
		if [ "${PROFILE##*/}" = "config.fish" ]; then
			printf 'set -gx HCC_ROOT %s\n' "$ROOT"
			printf 'set -gx PATH $HCC_ROOT/bin $PATH\n'
		else
			printf 'export HCC_ROOT="%s"\n' "$ROOT"
			printf 'export PATH="$HCC_ROOT/bin:$PATH"\n'
		fi
	} >> "$PROFILE"
	info "added ${BOLD}HCC_ROOT${RESET} and ${BOLD}\$HCC_ROOT/bin${RESET} to ${PROFILE}"
fi

if already_on_path; then
	info "run it: ${BOLD}${BIN} --help${RESET}"
else
	warn "${BIN_DIR} is not on your PATH in this shell yet."
	printf '\n  Open a new terminal, or run:\n\n' >&2
	printf '    export HCC_ROOT="%s"\n    export PATH="$HCC_ROOT/bin:$PATH"\n\n' "$ROOT" >&2
	info "then run: ${BOLD}${BIN} --help${RESET}  (or use the full path: ${DEST})"
fi
