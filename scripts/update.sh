#!/bin/sh
# Also works for installed releases without the update command.
set -eu
REPO="Subaru486desuwa/micu-image-mcp"
fail() { printf 'micu-image-mcp updater: %s\n' "$*" >&2; exit 1; }
target=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --binary-path)
            [ "$#" -ge 2 ] || fail "--binary-path requires a path"
            target="$2"; shift 2 ;;
        --help|-h)
            printf '%s\n' 'Usage: update.sh [--binary-path /path/to/installed/micu-image-mcp]'
            exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done
case "$(uname -s):$(uname -m)" in
    Darwin:arm64|Darwin:aarch64) asset="micu-image-mcp-macos-arm64" ;;
    Darwin:x86_64) asset="micu-image-mcp-macos-x86_64" ;;
    Linux:x86_64|Linux:amd64) asset="micu-image-mcp-linux-x86_64" ;;
    *) fail "no release binary for this platform; use the README source build instructions" ;;
esac
if [ -z "$target" ]; then
    case "$(uname -s)" in
        Darwin) target="$HOME/Library/Application Support/micu-image-mcp/bin/micu-image-mcp" ;;
        Linux) target="${XDG_DATA_HOME:-$HOME/.local/share}/micu-image-mcp/bin/micu-image-mcp" ;;
    esac
fi
[ -f "$target" ] && [ ! -L "$target" ] || fail "target must be an installed regular file: $target"
command -v curl >/dev/null 2>&1 || fail "curl is required"
parent="$(dirname "$target")"
lock="$target.update-lock"
mkdir "$lock" 2>/dev/null || fail "another update is running (lock: $lock)"
tmp_dir=""
cleanup() {
    [ -z "$tmp_dir" ] || rm -rf "$tmp_dir"
    rmdir "$lock" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
tmp_dir="$(mktemp -d "$parent/.micu-update-XXXXXX")"
# Resolve latest once, then pin both assets to that exact stable tag.
release_url="$(curl -fsSL --proto '=https' --proto-redir '=https' --max-time 20 \
    -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")"
case "$release_url" in
    "https://github.com/$REPO/releases/tag/"*) tag="${release_url##*/}" ;;
    *) fail "unexpected release URL" ;;
esac
[ "$(printf '%s' "$tag" | sed -n '/^v[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$/p')" = "$tag" ] \
    || fail "latest release is not a stable version"
base="https://github.com/$REPO/releases/download/$tag"
binary="$tmp_dir/$asset"
checksums="$tmp_dir/SHA256SUMS"
printf 'Downloading %s (%s)...\n' "$tag" "$asset" >&2
curl -fsSL --proto '=https' --proto-redir '=https' --max-time 120 --max-filesize 1048576 \
    -o "$checksums" "$base/SHA256SUMS"
curl -fsSL --proto '=https' --proto-redir '=https' --max-time 120 --max-filesize 134217728 \
    -o "$binary" "$base/$asset"
[ "$(wc -c < "$binary")" -le 134217728 ] || fail "binary exceeds size limit"
[ "$(wc -c < "$checksums")" -le 1048576 ] || fail "checksums exceed size limit"
expected="$(awk -v name="$asset" '$2 == name { print $1 }' "$checksums")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$binary" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$binary" | awk '{ print $1 }')"
else
    fail "sha256sum or shasum is required"
fi
[ "$actual" = "$expected" ] || fail "SHA-256 verification failed; existing binary preserved"
chmod 700 "$binary"
version="$("$binary" version)"
[ "v$version" = "$tag" ] || fail "binary version does not match release tag"
cp "$target" "$tmp_dir/previous"
chmod 700 "$tmp_dir/previous"
mv -f "$tmp_dir/previous" "$target.previous"
mv -f "$binary" "$target"
printf 'Updated to %s. Configuration, credentials, images and sessions preserved.\nBackup: %s.previous\nReconnect MCP, then confirm the version with server_info.\n' "$tag" "$target"
