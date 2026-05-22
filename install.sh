#!/bin/sh
# shellcheck shell=dash
# shellcheck disable=SC2039 # local is non-POSIX but supported in dash/bash/zsh/ksh
#
# wsctl installer — fetches a prebuilt binary from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/priyanshujain/workstation/main/install.sh | sh
#
# Environment variables:
#   WSCTL_VERSION       Version tag to install (e.g. v0.1.0). Default: latest release.
#   WSCTL_INSTALL_DIR   Directory to install into. Default: $XDG_BIN_HOME, else $HOME/.local/bin.
#   WSCTL_NO_VERIFY     Set to 1 to skip SHA256 verification (not recommended).

set -eu

REPO="priyanshujain/workstation"
BIN_NAME="wsctl"

say() { printf 'install: %s\n' "$1"; }
warn() { printf 'install: warning: %s\n' "$1" >&2; }
err() { printf 'install: error: %s\n' "$1" >&2; exit 1; }

need_cmd() {
    if ! command -v "$1" >/dev/null 2>&1; then
        err "required command not found: $1"
    fi
}

detect_target() {
    _os="$(uname -s)"
    _arch="$(uname -m)"

    case "$_os" in
        Darwin) ;;
        *) err "unsupported OS: $_os (only macOS is supported for now)" ;;
    esac

    case "$_arch" in
        arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
        x86_64) TARGET="x86_64-apple-darwin" ;;
        *) err "unsupported architecture: $_arch" ;;
    esac
}

resolve_version() {
    if [ -n "${WSCTL_VERSION:-}" ]; then
        VERSION="$WSCTL_VERSION"
        case "$VERSION" in v*) ;; *) VERSION="v$VERSION" ;; esac
        return
    fi
    say "resolving latest release..."
    _api_url="https://api.github.com/repos/$REPO/releases/latest"
    VERSION="$(curl -fsSL "$_api_url" \
        | grep -E '"tag_name"[[:space:]]*:' \
        | head -n1 \
        | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
    if [ -z "$VERSION" ]; then
        err "could not determine latest version (set WSCTL_VERSION to override)"
    fi
}

resolve_install_dir() {
    if [ -n "${WSCTL_INSTALL_DIR:-}" ]; then
        INSTALL_DIR="$WSCTL_INSTALL_DIR"
    elif [ -n "${XDG_BIN_HOME:-}" ]; then
        INSTALL_DIR="$XDG_BIN_HOME"
    elif [ -n "${HOME:-}" ]; then
        INSTALL_DIR="$HOME/.local/bin"
    else
        err "cannot determine install directory (HOME is unset)"
    fi
}

download() {
    _url="$1"
    _out="$2"
    curl -fsSL --proto '=https' --tlsv1.2 "$_url" -o "$_out"
}

verify_sha256() {
    _file="$1"
    _expected="$2"
    if command -v shasum >/dev/null 2>&1; then
        _actual="$(shasum -a 256 "$_file" | awk '{print $1}')"
    elif command -v sha256sum >/dev/null 2>&1; then
        _actual="$(sha256sum "$_file" | awk '{print $1}')"
    else
        err "neither shasum nor sha256sum is available"
    fi
    if [ "$_actual" != "$_expected" ]; then
        err "checksum mismatch: expected $_expected, got $_actual"
    fi
}

main() {
    need_cmd uname
    need_cmd curl
    need_cmd tar
    need_cmd mktemp
    need_cmd install

    detect_target
    resolve_version
    resolve_install_dir

    _version_no_v="${VERSION#v}"
    _archive="${BIN_NAME}-${_version_no_v}-${TARGET}.tar.gz"
    _base="https://github.com/$REPO/releases/download/$VERSION"

    say "installing $BIN_NAME $VERSION ($TARGET) -> $INSTALL_DIR"

    _tmp="$(mktemp -d)"
    # shellcheck disable=SC2064
    trap "rm -rf '$_tmp'" EXIT INT TERM

    say "downloading $_archive"
    download "$_base/$_archive" "$_tmp/$_archive"

    if [ "${WSCTL_NO_VERIFY:-0}" != "1" ]; then
        say "verifying checksum"
        download "$_base/$_archive.sha256" "$_tmp/$_archive.sha256"
        _expected="$(awk '{print $1}' "$_tmp/$_archive.sha256")"
        verify_sha256 "$_tmp/$_archive" "$_expected"
    else
        warn "skipping checksum verification (WSCTL_NO_VERIFY=1)"
    fi

    say "extracting"
    tar -xzf "$_tmp/$_archive" -C "$_tmp"

    if [ ! -f "$_tmp/$BIN_NAME" ]; then
        err "archive did not contain expected binary: $BIN_NAME"
    fi

    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$_tmp/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"

    say "installed $INSTALL_DIR/$BIN_NAME"

    case ":$PATH:" in
        *":$INSTALL_DIR:"*) ;;
        *)
            warn "$INSTALL_DIR is not on your PATH"
            warn "add this to your shell profile (~/.zshrc, ~/.bashrc, etc.):"
            # shellcheck disable=SC2016 # $PATH is meant to be literal in user-facing output
            printf '\n    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR" >&2
            ;;
    esac

    say "run \`$BIN_NAME --help\` to get started"
}

main "$@"
