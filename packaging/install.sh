#!/bin/sh
# Install watt-start from the latest GitHub release.
#
#   curl -fsSL https://github.com/gusahlg/watt-start/releases/latest/download/install.sh | sh
#
# Default: the current user, under ~/.local (no root). Pass --system to install
# into /usr/local instead; that is the only way this script will run as root.
set -eu

repo="gusahlg/watt-start"
system=0

usage() {
    printf '%s\n' \
        "Usage: install.sh [--system]" \
        "" \
        "Install the latest watt-start release for this machine's CPU." \
        "By default that is ~/.local. --system installs into /usr/local and is" \
        "required when running as root."
}

while [ $# -gt 0 ]; do
    case $1 in
        --system)
            system=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown argument: %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [ "$(id -u)" -eq 0 ] && [ "$system" -ne 1 ]; then
    printf '%s\n' \
        "Refusing to install as root into a home directory." \
        "Re-run as your user, or pass --system to install into /usr/local." >&2
    exit 1
fi

os=$(uname -s)
if [ "$os" != "Linux" ]; then
    printf 'install.sh is for Linux (this is %s). On Windows, run install.ps1.\n' "$os" >&2
    exit 1
fi

machine=$(uname -m)
case $machine in
    x86_64 | amd64)
        target="x86_64-linux-gnu"
        ;;
    aarch64 | arm64)
        target="aarch64-linux-gnu"
        ;;
    *)
        printf 'No watt-start build for %s. Releases ship x86_64 and aarch64.\n' "$machine" >&2
        exit 1
        ;;
esac


if [ "$system" -eq 1 ]; then
    bindir="/usr/local/bin"
    appdir="/usr/local/share/applications"
    icondir="/usr/local/share/icons"
else
    if [ -z "${HOME:-}" ]; then
        printf '%s\n' "HOME is not set." >&2
        exit 1
    fi
    bindir="$HOME/.local/bin"
    appdir="$HOME/.local/share/applications"
    icondir="$HOME/.local/share/icons"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Run from an unpacked release tarball (./install.sh next to the binary), install
# those files. Piped from curl, $0 is the shell, so download the latest release.
here=""
case $0 in
    */install.sh | install.sh)
        here=$(cd "$(dirname "$0")" && pwd)
        ;;
esac

if [ -n "$here" ] && [ -f "$here/watt-start" ] && [ -f "$here/watt-start.desktop" ]; then
    src="$here"
    version=$("$src/watt-start" --version 2>/dev/null | awk '{ print $2 }' || true)
    version=${version:-"(unpacked)"}
    printf 'Installing from %s\n' "$src"
else
    for tool in curl tar; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            printf '%s is required to download and unpack the release.\n' "$tool" >&2
            exit 1
        fi
    done

    # /releases/latest redirects to /releases/tag/v<version>.
    effective=$(
        curl -fsSL -o /dev/null -w '%{url_effective}' \
            "https://github.com/${repo}/releases/latest"
    )
    effective=${effective%/}
    tag=${effective##*/}
    tag=${tag%%\?*}
    case $tag in
        v*)
            version=${tag#v}
            ;;
        *)
            printf 'Could not resolve the latest release (got %s).\n' "$effective" >&2
            exit 1
            ;;
    esac

    archive="watt-start-${version}-${target}.tar.gz"
    url="https://github.com/${repo}/releases/download/${tag}/${archive}"
    printf 'Downloading %s\n' "$url"
    curl -fsSL --retry 3 -o "$tmp/watt-start.tar.gz" "$url"
    tar -xzf "$tmp/watt-start.tar.gz" -C "$tmp"
    src="$tmp/watt-start"
fi

if [ ! -f "$src/watt-start" ] || [ ! -f "$src/watt-start.desktop" ] || [ ! -f "$src/watt-start.png" ]; then
    printf '%s\n' "The archive is missing watt-start, watt-start.desktop or watt-start.png." >&2
    exit 1
fi

mkdir -p "$bindir" "$appdir" "$icondir"
exe="$bindir/watt-start"
icon="$icondir/watt-start.png"
desktop="$appdir/watt-start.desktop"

# Copy next to the target and rename over it: a running watt-start keeps its
# old inode instead of failing with "Text file busy".
cp "$src/watt-start" "$exe.new"
chmod 755 "$exe.new"
mv -f "$exe.new" "$exe"
cp "$src/watt-start.png" "$icon"
chmod 644 "$icon"

# Absolute Exec and Icon. A line copy, not sed, so a path may contain
# spaces, slashes or '&'.
: >"$tmp/desktop"
while IFS= read -r line || [ -n "$line" ]; do
    case $line in
        Exec=*)
            # The desktop spec splits Exec on spaces; quote a path that has one.
            case $exe in
                *" "*) printf 'Exec="%s"\n' "$exe" ;;
                *) printf 'Exec=%s\n' "$exe" ;;
            esac
            ;;
        Icon=*)
            printf 'Icon=%s\n' "$icon"
            ;;
        *)
            printf '%s\n' "$line"
            ;;
    esac
done <"$src/watt-start.desktop" >"$tmp/desktop"
cp "$tmp/desktop" "$desktop"
chmod 644 "$desktop"

printf '\nInstalled watt-start %s\n' "$version"
printf '  binary   %s\n' "$exe"
printf '  launcher %s\n' "$desktop"
printf '  icon     %s\n' "$icon"
printf '\n'

case ":${PATH:-}:" in
    *":$bindir:"*) ;;
    *)
        printf 'Add %s to PATH. For example, in ~/.profile:\n' "$bindir"
        # \$PATH is literal: the line is for the user to paste into a profile.
        printf '  export PATH="%s:%s"\n\n' "$bindir" "\$PATH"
        ;;
esac

printf '%s\n' "Run: watt-start"
printf '%s\n' "The first game build compiles Project Watt Cubed and takes 15-40 minutes."
printf '%s\n' "That build needs git, a Vulkan driver and Rust 1.90 or newer."

if [ -f /etc/NIXOS ]; then
    printf '\n%s\n' "This is NixOS. The release binary cannot find libwayland or the Vulkan loader."
    printf '%s\n' "Install the flake instead: nix profile add github:gusahlg/watt-start"
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$appdir" >/dev/null 2>&1 || true
fi
