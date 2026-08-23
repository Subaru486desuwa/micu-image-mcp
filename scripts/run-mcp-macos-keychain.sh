#!/bin/sh
set -eu

if [ "$(/usr/bin/uname -s)" != "Darwin" ] || [ ! -x /usr/bin/security ]; then
  echo "micu-image MCP: macOS Keychain is required by this launcher" >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(/usr/bin/dirname -- "$0")" && /bin/pwd)
rust_binary=${MICU_MCP_BINARY:-""}
python_bin=${MICU_MCP_PYTHON:-"$script_dir/../.venv/bin/python"}
server_file=${MICU_MCP_SERVER:-"$script_dir/../server.py"}
keychain_service=${MICU_KEYCHAIN_SERVICE:-"ai.micuapi.mcp"}
keychain_account=${MICU_KEYCHAIN_ACCOUNT:-"${USER:-$(/usr/bin/id -un)}"}

if [ -n "$rust_binary" ]; then
  if [ ! -x "$rust_binary" ]; then
    echo "micu-image MCP: Rust binary is missing or not executable: $rust_binary" >&2
    exit 1
  fi
else
  if [ ! -x "$python_bin" ]; then
    echo "micu-image MCP: Python runtime is missing: $python_bin" >&2
    exit 1
  fi
  if [ ! -f "$server_file" ]; then
    echo "micu-image MCP: server entry point is missing: $server_file" >&2
    exit 1
  fi
fi

if ! MICU_API_KEY=$(
  /usr/bin/security find-generic-password \
    -a "$keychain_account" \
    -s "$keychain_service" \
    -w 2>/dev/null
); then
  echo "micu-image MCP: API key not found in macOS Keychain" >&2
  exit 1
fi
if [ -z "$MICU_API_KEY" ]; then
  echo "micu-image MCP: API key in macOS Keychain is empty" >&2
  exit 1
fi

export MICU_API_KEY
if [ -n "$rust_binary" ]; then
  exec "$rust_binary" serve
fi
exec "$python_bin" "$server_file"
