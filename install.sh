#!/bin/sh
# install the pith toolchain from a github release:
#
#   curl -fsSL https://pithlang.com/install.sh | sh
#
# PITH_HOME     where to install (default: ~/.pith)
# PITH_VERSION  a release tag such as v0.2.15 (default: the latest release)
set -eu

say() { printf 'pith: %s\n' "$*"; }
fail() { printf 'pith: %s\n' "$*" >&2; exit 1; }

main() {
    repo=kacy/pith
    home=${PITH_HOME:-$HOME/.pith}

    case "$(uname -s)" in
        Linux) os=linux ;;
        Darwin) os=macos ;;
        *) fail "no prebuilt toolchain for $(uname -s); build from source instead" ;;
    esac
    case "$(uname -m)" in
        x86_64 | amd64) arch=x86_64 ;;
        arm64 | aarch64) arch=aarch64 ;;
        *) fail "no prebuilt toolchain for $(uname -m); build from source instead" ;;
    esac
    # a shell running under rosetta reports x86_64 on an arm mac
    if [ "$os" = macos ] && [ "$arch" = x86_64 ] &&
        [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
        arch=aarch64
    fi

    asset="pith-$os-$arch.tar.gz"
    if [ -n "${PITH_VERSION:-}" ]; then
        ver=$PITH_VERSION
        case "$ver" in
            *[!A-Za-z0-9._+-]* | *..* | */*)
                fail "invalid PITH_VERSION: $ver"
                ;;
            v*) ;;
            *) ver="v$ver" ;;
        esac
        base="https://github.com/$repo/releases/download/$ver"
    else
        base="https://github.com/$repo/releases/latest/download"
    fi

    if command -v curl >/dev/null 2>&1; then
        fetch() { curl -fsSL --proto '=https' --tlsv1.2 "$1" -o "$2"; }
    elif command -v wget >/dev/null 2>&1; then
        fetch() { wget -qO "$2" --https-only --secure-protocol=TLSv1_2 "$1"; }
    else
        fail "needs curl or wget"
    fi

    tmp=$(mktemp -d)
    stage=""
    cleanup() {
        rm -rf "$tmp"
        if [ -n "$stage" ]; then
            rm -rf "$stage"
        fi
    }
    trap cleanup EXIT INT TERM HUP

    say "downloading $asset"
    fetch "$base/$asset" "$tmp/$asset" || fail "could not download $base/$asset"
    fetch "$base/$asset.sha256" "$tmp/$asset.sha256" ||
        fail "could not download $base/$asset.sha256"

    expected=$(cut -d ' ' -f 1 "$tmp/$asset.sha256")
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)
    else
        fail "needs sha256sum or shasum to verify the download"
    fi
    [ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

    # unpack directly into a staging dir inside PITH_HOME so each entry swap is
    # a same-filesystem rename rather than a cross-filesystem copy from /tmp
    mkdir -p "$home"
    stage="$home/.install-staging.$$"
    rm -rf "$stage"
    mkdir -p "$stage"
    tar -C "$stage" -xzf "$tmp/$asset" --strip-components=1

    # verify the binary can execute on this host (catches glibc/musl mismatch)
    # before replacing an existing installation
    version_line=$("$stage/bin/pith" version 2>/dev/null | head -n 1 || true)
    [ -n "$version_line" ] || fail "downloaded binary cannot run on this host; build from source instead"

    # replace only what a release ships, so a PITH_HOME that holds other files
    # keeps them
    for entry in bin lib std LICENSE README.md; do
        [ -e "$stage/$entry" ] || continue
        rm -rf "${home:?}/$entry"
        mv "$stage/$entry" "$home/$entry"
    done
    rm -rf "$stage"
    stage=""

    say "installed $version_line in $home"

    if ! command -v cc >/dev/null 2>&1 && [ -z "${CC:-}" ]; then
        if [ "$os" = macos ]; then
            say "pith links programs with the system c compiler; run: xcode-select --install"
        else
            say "pith links programs with the system c compiler; install gcc or clang"
        fi
    fi

    case ":$PATH:" in
        *":$home/bin:"*) ;;
        *)
            case "${SHELL:-}" in
                */zsh) rc="~/.zshrc" ;;
                */bash) rc="~/.bashrc" ;;
                */fish) rc="" ;;
                *) rc="~/.profile" ;;
            esac
            if [ -n "$rc" ]; then
                say "add pith to your PATH by adding this line to $rc:"
                printf '\n    export PATH="%s/bin:$PATH"\n\n' "$home"
            else
                say "add pith to your PATH with:"
                printf '\n    fish_add_path %s/bin\n\n' "$home"
            fi
            ;;
    esac
}

main "$@"
