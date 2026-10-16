# Micu Image MCP

[简体中文](README.md) · [English](README.en.md)

Use [Micu's](https://www.micuapi.ai) image API in Claude Code, Claude Desktop, Codex, Cursor, and other MCP clients. Generate images, edit them across multiple turns, apply one instruction to a batch, or create an image from several references.

For multi-turn editing, the server saves image revisions, successful instructions, and current constraints. Reuse a session ID to continue editing a previous image. Each separate image task has its own session.

![Micu Image MCP v0.4.1 session memory poster: continuing to edit the same kitten](docs/assets/memory/context-memory-poster.png)

[Installation](#installation) · [Multi-turn editing](#session-memory) · [Models and quality](#models-and-quality) · [Image sizes](#image-sizes) · [Testing and validation](#testing-and-validation) · [v0.4.3 release](https://github.com/Subaru486desuwa/micu-image-mcp/releases/tag/v0.4.3)

## Session memory

Generate a kitten, then change its flight equipment. Start a separate session for a seaside portrait, then return to the kitten and change the background to a sunset. The client passes the matching session ID, and the server includes that revision's image, successful editing history, and current constraints.

![Four real MCP calls: kitten session A at r1 and r2, seaside session B at r1, then back to A at r3](docs/assets/memory/four-turn-demo.png)

| Your request | Image session | What the server does |
| --- | --- | --- |
| "Generate a kitten parachuting." | A · r1 | Creates a session and saves the first image and description |
| "Give it different flight equipment." | A · r2 | Uploads A's previous image with the subject requirements and current constraints |
| "Create a separate seaside portrait." | B · r1 | Creates B with its own image and constraints |
| "Back to the kitten. Change the background to a sunset." | A · r3 | Continues editing from A's successful history |

These images came from live validation on 2026-10-09 in Osaka, using OpenCode, DeepSeek Flash, and this server to call Micu's main endpoint. The seaside request failed once and succeeded after a prompt adjustment; the failure did not advance the session revision. The second image depicted a rigid hang glider. A later manual instruction specified a soft wing, producing A · r4, the image used in the poster. The poster and four-turn demo identify their revisions separately. See the [image provenance notes](docs/assets/memory/README.md).

### Continue an editing session

1. Pass `session_id="new"` to `image_generate` or `image_edit` on the first call. The client saves the returned `session.id`. Generation with a session requires `n=1`.
2. Pass the same ID and your next instruction to continue. Omitted `model`, `size`, and `quality` values inherit the selected revision's settings. The server uploads that revision's image with its editing context.
3. Use `"new"` for another image task and the original ID to return to an earlier one. `A` and `B` above are short labels; actual IDs have the form `edit_<32 hexadecimal characters>`.
4. Sessions live on the computer running the MCP server. Keep the ID, output root, and corresponding images to resume after a restart. Set `parent_revision` to branch from an older revision.

The client or model chooses the session. The server reads the supplied ID; it does not receive the client's complete chat history. Omitting `session_id` makes a one-shot call without saving a session. A revision advances only after the image is saved and the session record is committed. A second call to a session already being edited returns a busy error.

See the [multi-turn editing guide](docs/multi-turn-editing.md) and [session storage guide](docs/session-storage.md) for supported behavior and storage management. Linked guides and the text in the example images are in Chinese unless otherwise indicated.

[Download the poster PNG](docs/assets/memory/context-memory-poster.png) · [Editable SVG](docs/assets/memory/context-memory-poster.svg) · [Full-size four-turn demo](docs/assets/memory/four-turn-demo.png)

---

## Tools

| Tool | Description |
| --- | --- |
| `image_generate` | Generates images from text using Flare by default; continues editing when given an existing session ID |
| `image_edit` | Edits an image using one reference and an instruction; defaults to Sunburst and supports a mask |
| `image_batch_edit` | Applies the same instruction to each input image separately; defaults to serial processing with Sunburst |
| `image_multi_reference` | Creates one image using 2 to 10 reference images together; defaults to Sunburst |
| `server_info` | Reports the running version, API endpoint, models, size rules, retry policy, and safety limits |

Before the first image call, ask your client to call `server_info` to check the configuration and tool capabilities.

---

## Usage guide

The [MCP usage guide](docs/MCP使用教程.md) covers setup for Cursor, Claude Code, and Codex, tool selection, size rules, environment variables, and image download problems caused by Clash or Surge fake-ip settings.

---

## Claude Desktop compatibility

v0.4.1 fixed the `Connected · 0 tools` problem reported in [issue #11](https://github.com/Subaru486desuwa/micu-image-mcp/issues/11). Upgrade the executable your client actually starts, reconnect the MCP server, and call `server_info` to confirm the version.

Desktop's Code mode reads `~/.claude.json`. Regular Chat mode uses a separate `claude_desktop_config.json`, which the installer does not currently update. The [compatibility guide](docs/claude-desktop-compatibility.md) explains the cause, configuration for both modes, and test coverage.

---

## Models and quality

All four image tools accept `gpt-image-2.5-flare-openai`, `gpt-image-2.5-sunburst`, `gpt-image-2`, and `gpt-image-2-openai`. Text-to-image generation defaults to Flare. Editing, batch editing, and multi-reference generation default to Sunburst. Model availability depends on the response from `/v1/models` for your credentials.
The old ID `gpt-image-2.5-flare` maps automatically to `gpt-image-2.5-flare-openai`, including existing `MICU_MODEL` settings and saved editing sessions.
Flare Images requests omit `response_format` and save the base64 image or URL returned by the backend. A download or parsing failure does not trigger another generation with a different format. All image tools use `/v1/images/generations` or `/v1/images/edits`, never the Responses API.

On 2026-10-16, the v0.4.3 release candidate completed default Flare generation and a session edit after restarting MCP against Micu's main endpoint. Both Images requests returned HTTP 200 and 1024×1024 images, with `quality` and `response_format` omitted.

Live tests of 1K generation and single-image editing on Micu's main endpoint on 2026-10-09 produced these results.

| Model | Accepted `quality` values in those tests | Limits |
| --- | --- | --- |
| Flare / Sunburst | `low`, `auto`, or omitted | `medium`, `high`, `xhigh`, and `max` returned HTTP 400; the upstream service reported support for `low` only |
| `gpt-image-2` | `low`, `medium`, `high`, `auto`, or omitted | 2K/4K requests switch to `gpt-image-2-openai`; confirm that your credentials support it |
| `gpt-image-2-openai` | Not tested | The model was absent from the model list for those credentials |

See the [quality validation record](docs/session-storage.md#在线质量档位验证) for details. These results describe the endpoint and credentials used at that time, with Flare using its old ID. The tools retain their full parameter options for other endpoints or future support. See [image sizes](#image-sizes) for routing behavior.

The Grok implementation remains in the repository but is disabled. The tools reject Grok models.

---

> The MCP server sends Chinese text as UTF-8 JSON. If you write your own PowerShell test scripts, set `$OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new()` first so older PowerShell versions do not replace Chinese characters with `?` in pipelines.

## Installation

The server is a single Rust executable. Running it without arguments starts the STDIO MCP service. Python is not required. Product code, tests, and engineering tools on `main` use Rust. The historical Python implementation remains on the [`python-reference`](https://github.com/Subaru486desuwa/micu-image-mcp/tree/python-reference) branch and is excluded from current builds and releases.

### Install with a script

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.ps1 | iex
```

The script detects your platform, downloads the executable from the latest GitHub Release, verifies it against that release's `SHA256SUMS`, and runs `install --yes`. By default, the installer adds the `micu-image` configuration to Codex and Claude Code. Claude Code uses `~/.claude.json`. Claude Desktop's regular Chat mode needs [separate configuration](docs/claude-desktop-compatibility.md).

The installer does not write API keys to client configuration files. It checks `MICU_API_KEY` and the system credential store first. If neither has a key on first installation, it prompts once in an interactive terminal without echoing your input. After validating the `sk-` prefix, a length of 20 to 512 characters, and the ASCII character set, it saves the key in macOS Keychain, Windows Credential Manager, or Linux Secret Service. The MCP server reads the stored key at startup.

The default API endpoint is `https://www.micuapi.ai`. Use `--baseurl` or `MICU_BASEURL` to configure another endpoint.

To configure only Codex on macOS / Linux, pass `--no-claude` to the installer:

```bash
curl -fsSL https://raw.githubusercontent.com/Subaru486desuwa/micu-image-mcp/main/scripts/install.sh \
  | sh -s -- --no-claude
```

You can review [`scripts/install.sh`](scripts/install.sh) and [`scripts/install.ps1`](scripts/install.ps1) before running them.

### Download the executable manually

Download the asset for your platform from the [latest release](https://github.com/Subaru486desuwa/micu-image-mcp/releases/latest) and verify `SHA256SUMS`:

```bash
chmod +x /absolute/path/micu-image-mcp   # macOS/Linux
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
/absolute/path/micu-image-mcp install --yes
```

`install` copies the executable to a fixed directory for your user account and configures Codex and Claude Code to use that copy. Moving the source repository or running `cargo clean` does not affect the installed server.

- macOS: `~/Library/Application Support/micu-image-mcp/bin/micu-image-mcp`
- Linux: `~/.local/share/micu-image-mcp/bin/micu-image-mcp`
- Windows: `%LOCALAPPDATA%\micu-image-mcp\bin\micu-image-mcp.exe`

Common commands:

```bash
micu-image-mcp                 # Same as serve: STDIO MCP
micu-image-mcp serve
micu-image-mcp install --yes --no-claude
micu-image-mcp install --yes --binary-path /path/to/downloaded/binary
micu-image-mcp install --yes --dev --binary-path "$PWD/target/release/micu-image-mcp"
micu-image-mcp reset --yes
micu-image-mcp doctor
micu-image-mcp version
```

### Build from source

Rust 1.88 or newer is required. Use this method on platforms without a prebuilt asset, including Linux ARM64. For development, you can configure clients to use the build output directly:

```bash
cargo build --release
MICU_SAVE_DIR="$HOME/Pictures/micu-out" \
target/release/micu-image-mcp install --yes --dev \
  --binary-path "$PWD/target/release/micu-image-mcp"
```

### macOS Keychain

At startup, the executable reads the API key from macOS Keychain using a service and account name. For an existing custom Keychain item, set `MICU_KEYCHAIN_SERVICE` and `MICU_KEYCHAIN_ACCOUNT` to match it. This example creates an item manually and configures Codex:

```bash
security add-generic-password \
  -U -a "$USER" -s ai.micuapi.mcp \
  -l "Micu Image MCP API Key" \
  -T /usr/bin/security -w
```

```toml
[mcp_servers.micu-image]
command = "/Users/you/Library/Application Support/micu-image-mcp/bin/micu-image-mcp"
args = []

[mcp_servers.micu-image.env]
MICU_KEYCHAIN_SERVICE = "ai.micuapi.mcp"
MICU_KEYCHAIN_ACCOUNT = "your-macos-account"
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

Codex desktop, CLI, and IDE extensions share `~/.codex/config.toml`. Restart your client after changing it.

### Verify and roll back

After installation, ask your client to call `server_info`. Check the version, `available_models`, API endpoint, output root, and `api_key_configured`. You can also check configuration in a terminal or run an offline MCP check from the source repository:

```bash
micu-image-mcp doctor
cargo xtask smoke --binary /absolute/path/micu-image-mcp
```

To roll back installation changes, restore a trusted executable and the configuration backups created by the installer. Keep your images and sessions. See the [migration and rollback guide](docs/migration-from-python.md) and [Rust-only development guide](docs/rust-only-development.md) for details.

---

## Image sizes

All four supported models use these size checks:

- Width and height must be multiples of 16.
- Each edge must be between 256 and 3840 pixels, with an aspect ratio no greater than 3:1.
- Total pixel count must be between 655,360 and 8,294,400.

Routing and concurrency follow these rules:

- Single-image editing, batch editing, and multi-reference generation use `/v1/images/edits`.
- 2K/4K requests for `gpt-image-2` switch to `gpt-image-2-openai`.
- 2K/4K requests for Flare or Sunburst keep the selected model.
- 2K/4K generation forces `n=1`. In-process and cross-process locks serialize high-resolution requests.

Read each image's `saved.actual_size` for the dimensions actually saved. The generation tool returns `saved` as a list.

Common `size` values:

| Tier | Suggested values |
| --- | --- |
| 1K | `1024x1024`, `1280x720`, `720x1280`, `1024x1536`, `1536x1024` |
| 2K | `2048x2048`, `2048x1152`, `1152x2048` |
| 4K | `3840x2160`, `2160x3840` |

---

## Environment variables

| Variable | Default | Description |
| --- | --- | --- |
| `MICU_API_KEY` | Empty | API key for Micu's Image2 channel |
| `MICU_BASEURL` | `https://www.micuapi.ai` | Image API endpoint |
| `MICU_KEYCHAIN_SERVICE` | `micu-image-mcp` | Service name in the system credential store |
| `MICU_KEYCHAIN_ACCOUNT` | `image2-api-key` | Account name in the system credential store |
| `MICU_MODEL` | Empty | Optional global override; otherwise generation defaults to Flare and editing tools to Sunburst |
| `MICU_SAVE_DIR` | `~/Pictures/micu-out` | Default output directory |
| `MICU_SAVE_DIR_ROOT` | Same as output directory | Root directory restricting all output paths |
| `MICU_INPUT_ROOT` | Empty, unrestricted | Optional root for input images; prevents path and symlink escapes when set |
| `MICU_USE_SHELL_PROXY` | `0` | Set to `1` to read proxy settings from the process environment |
| `MICU_RESPONSE_FORMAT` | `auto` | Legacy models accept `auto`, `url`, or `b64_json`; Flare omits this parameter and saves the backend response |
| `MICU_TRUSTED_DOWNLOAD_HOSTS` | `oss.filenest.top` | Trusted download hosts, separated by commas |
| `MICU_ALLOW_FAKE_IP_DOWNLOAD` | `1` | Allows fake-ip downloads in 198.18.0.0/15 only for trusted hosts |

Path bases are fixed at server startup. Relative `MICU_SAVE_DIR` and tool `save_dir` paths resolve against the output root. When `MICU_INPUT_ROOT` is set, relative input paths resolve against it; otherwise they resolve against the startup working directory. Paths expand only `~`, `~/...`, and Windows `~\...`. The server rejects `~someone`.

## Manual configuration

Claude Code uses this JSON configuration:

```json
{
  "mcpServers": {
    "micu-image": {
      "command": "/absolute/path/micu-image-mcp",
      "args": [],
      "env": {
        "MICU_SAVE_DIR": "/Users/you/Pictures/micu-out",
        "MICU_SAVE_DIR_ROOT": "/Users/you/Pictures/micu-out"
      }
    }
  }
}
```

Codex uses this TOML configuration:

```toml
[mcp_servers.micu-image]
command = "/absolute/path/micu-image-mcp"
args = []

[mcp_servers.micu-image.env]
MICU_SAVE_DIR = "/Users/you/Pictures/micu-out"
MICU_SAVE_DIR_ROOT = "/Users/you/Pictures/micu-out"
```

Replace `command` with the actual executable path and adjust the output directories. For Windows paths in TOML, use single-quoted strings, such as `'C:\Users\you\AppData\Local\micu-image-mcp\bin\micu-image-mcp.exe'`. Double-quoted strings require escaped backslashes. The installer handles escaping and validates the configuration.

These JSON/TOML examples do not store API keys. Credentials come from the MCP process environment, the system credential store, or the tool's `api_key` parameter.

---

## Testing and validation

The current Rust test suite uses real MCP STDIO calls and local HTTP fixtures to check tool protocols, request parameters, image downloads, path limits, session recovery, failure handling, and installer configuration. Offline tests do not incur image fees. They cannot replace visual inspection of actual generated images. See [Rust-only development and validation](docs/rust-only-development.md) for the full check commands.

Records from the v0.4.1 release are available in the [debugging notes](docs/v0.4.1-debugging-notes.md), [native client validation report](docs/harness-validation.md), [CI run at that time](https://github.com/Subaru486desuwa/micu-image-mcp/actions/runs/37884013213), and [release workflow](https://github.com/Subaru486desuwa/micu-image-mcp/actions/runs/37884913631). These historical records include Python tests and performance data from before the Rust-only migration. They do not describe current test counts or the latest platform validation.

That round of MacBook validation passed checks for the five tools, protocols, and `server_info`. Image generation through the Claude Desktop window still requires user acceptance.

---

## Engineering documentation

- [Rust-only development and validation](docs/rust-only-development.md)
- [Historical Rust / Python benchmarks](docs/rust-benchmark.md)
- [v0.4.1 debugging notes](docs/v0.4.1-debugging-notes.md)
- [Native client validation and timing](docs/harness-validation.md)
- [Rust compatibility matrix](docs/rust-compatibility-matrix.md)
- [Security review](docs/rust-security-review.md)
- [Python to Rust migration and rollback](docs/migration-from-python.md)
- [Contributing and local validation](CONTRIBUTING.md)

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=Subaru486desuwa/micu-image-mcp&type=Date)](https://star-history.com/#Subaru486desuwa/micu-image-mcp&Date)
