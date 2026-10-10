#!/bin/sh
set -eu

if [ "$(/usr/bin/uname -s)" != Darwin ]; then
  echo 'micu-image MCP: this launcher requires macOS' >&2
  exit 1
fi

rust_binary=${MICU_MCP_BINARY:-"$HOME/.local/bin/micu-image-mcp"}
if [ ! -x "$rust_binary" ]; then
  echo 'micu-image MCP: install the Rust binary or set MICU_MCP_BINARY' >&2
  exit 1
fi
# The Rust configuration loader reads Keychain without exposing the key to this shell.
exec "$rust_binary" serve
