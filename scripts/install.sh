#!/bin/sh
set -eu

REPO="Subaru486desuwa/micu-image-mcp"
LATEST_BASE="https://github.com/${REPO}/releases/latest/download"

fail() {
    printf 'micu-image-mcp installer: %s\n' "$*" >&2
    exit 1
}

command -v curl >/dev/null 2>&1 || fail "curl is required"

case "$(uname -s)" in
    Darwin) platform="macos" ;;
    Linux) platform="linux" ;;
    *) fail "unsupported OS: $(uname -s). Use the Windows PowerShell installer on Windows." ;;
esac

case "$(uname -m)" in
    x86_64|amd64) arch="x86_64" ;;
    arm64|aarch64) arch="arm64" ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
esac

if [ "$platform" = "linux" ] && [ "$arch" != "x86_64" ]; then
    fail "Linux release binaries currently support x86_64 only"
fi

for arg in "$@"; do
    case "$arg" in
        --dev|--binary-path|--binary-path=*)
            fail "$arg cannot be used with the one-line installer"
            ;;
    esac
done

asset="micu-image-mcp-${platform}-${arch}"
tmp_dir="$(mktemp -d 2>/dev/null || mktemp -d -t micu-image-mcp)"
trap 'rm -rf "$tmp_dir"' EXIT HUP INT TERM

binary="$tmp_dir/$asset"
checksums="$tmp_dir/SHA256SUMS"

printf 'Downloading %s...\n' "$asset" >&2
curl -fL --retry 3 --connect-timeout 15 -o "$checksums" "$LATEST_BASE/SHA256SUMS"
curl -fL --retry 3 --connect-timeout 15 -o "$binary" "$LATEST_BASE/$asset"

expected="$(awk -v name="$asset" '$2 == name { print $1; exit }' "$checksums")"
[ -n "$expected" ] || fail "SHA256SUMS does not contain $asset"

if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$binary" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$binary" | awk '{ print $1 }')"
else
    fail "sha256sum or shasum is required to verify the release binary"
fi

[ "$actual" = "$expected" ] || fail "SHA-256 verification failed for $asset"

chmod 700 "$binary"
version="$($binary version)"
printf 'Verified micu-image-mcp v%s (%s).\n' "$version" "$asset" >&2

if [ -t 2 ] && [ -r /dev/tty ]; then
    "$binary" install --yes "$@" < /dev/tty
else
    "$binary" install --yes "$@"
fi
