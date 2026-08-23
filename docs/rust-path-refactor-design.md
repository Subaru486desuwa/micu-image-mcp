# Rust path refactor design and audit

Status: implemented and locally verified in `aec146b`

Baseline: `e81b92e2e20417cb482dab48c3673d468f107e52`

Related issue: GitHub #4, Windows backslashes rendered inside TOML basic strings

This refactor treats path handling as a deep module with a small typed interface. It must not
change the five MCP tools, schemas, model/size/quality/n rules, routes, retries, concurrency,
multipart fields, or public response fields.

## 1. Current path inventory

| Path kind | Source | Current relative base | Must exist | May create | Security root / policy | Current consumers |
|---|---|---|---:|---:|---|---|
| Running Rust executable | `std::env::current_exe()` | n/a | yes | no | executable itself | installer, doctor |
| Installed Rust binary | not implemented; config points at current executable | n/a | yes | installer should create | per-user data-local install directory | Codex/Claude command |
| Codex config | `dirs::home_dir()/.codex/config.toml` | n/a | no | yes | fixed client path; temp/backup in `.codex` | installer/reset |
| Claude config | `dirs::home_dir()/.claude.json` | n/a | no | yes | fixed client path; temp/backup beside file | installer/reset |
| Home | `HOME` / `USERPROFILE` / `dirs::home_dir()` | n/a | yes | no | captured once | runtime config, installer, tilde expansion |
| Startup working directory | repeated `std::env::current_dir()` calls | process cwd at each call | yes | no | none when input root is absent | config, save path, input path, installer |
| Save root | `MICU_SAVE_DIR_ROOT`, otherwise `home/Pictures/micu-out` | relative values currently use process cwd | no | yes | itself after create + canonicalize | `Config`, `Storage` |
| Default save directory | `MICU_SAVE_DIR`, otherwise save root | relative values currently use process cwd | no | yes | save root | `Storage::resolve_save_dir` |
| Tool `save_dir` | MCP UTF-8 string | currently process cwd | no | yes | save root | all four image tools through `Storage` |
| Input root | optional `MICU_INPUT_ROOT` | relative values currently use process cwd | yes when configured | no | itself after canonicalize | image validation |
| Tool input/mask/reference paths | MCP UTF-8 strings | process cwd, or only checked under input root after expansion | yes | no | input root when configured | image validation, multipart snapshots |
| Lock file | hard-coded `home/.cache/micu-image/bigsize.lock` | n/a | no | yes | parent cache directory | `HttpExecutor` / `BigRequestGate` |
| Output temp | generated `.micu-*.tmp` | resolved output directory | no | yes | capability-open save root | `Storage` |
| Output final | basename + decoded extension | resolved output directory | no | yes, no-clobber | capability-open save root | `Storage`, all tools |
| Input upload snapshot | `tempfile::NamedTempFile::new()` | OS temp directory | no | yes | private temp file | image validation, multipart retry |
| Download stream temp | same as output temp | resolved output directory | no | yes | capability-open save root | `OutputSaver` / `Storage` |
| Config backup/temp | original config parent | n/a | no | yes | original config directory | installer atomic writer |
| Benchmark/contract roots | injected `tempfile` paths | explicit absolute temp roots | no | yes | each test root | test adapters |

### Current path call sites

- `src/config.rs` reads home and current cwd, parses save/input roots, and also chooses the lock.
- `src/storage.rs` repeats home/cwd expansion at each tool call, creates/canonicalizes the save root,
  performs directory resolution, streams bytes, validates images, and commits output files.
- `src/validation/image.rs` repeats home/cwd expansion and root canonicalization for each input.
- `src/installer/mod.rs` independently reads home/current cwd/current exe, constructs client config
  paths, serializes TOML/JSON, creates backups/temp files, and writes the executable path.
- `src/locks.rs` consumes a path from runtime config and creates its parent/file asynchronously.
- `src/tools/common.rs` converts returned paths with `to_string_lossy()`.
- tool test fixtures independently rebuild environment path maps and frequently call
  `to_string_lossy()`.
- the Python reference uses `Path.home()/.cache/micu-image/bigsize.lock`; Rust currently uses the
  same location, so that default must remain shared during coexistence.

## 2. Audit findings

1. **Cwd drift:** save/input/installer code calls `current_dir()` in multiple modules and at
   different times. A client starting the same binary with another cwd can change relative-path
   meaning.
2. **Relative save mismatch:** tool `save_dir="child"` is currently resolved against cwd and then
   usually rejected instead of being resolved below the configured save root.
3. **Repeated tilde parsing:** config, storage, input validation, and installer each implement a
   slightly different `~` rule. Installer's `strip_prefix("~")` also accepts unsupported
   `~someone`-style paths.
4. **Path/data mixing:** runtime `Config` contains both HTTP/business configuration and filesystem
   topology, which makes tests depend on ambient home/cwd.
5. **Installer breadth:** one 487-line module owns CLI orchestration, secret collection, binary
   discovery, TOML, JSON, backup, atomic I/O, permission changes, and doctor logic.
6. **No stable install:** Codex/Claude point at the executable used to run install. A
   `target/release` path breaks after repository moves or `cargo clean`.
7. **No write-read verification:** Rust already uses `toml_edit`, but it does not parse the exact
   temporary bytes and assert command/args/env round-trip before replacement. Claude JSON has the
   same missing verification.
8. **Lossy paths:** installer, multipart filename creation, returned saved paths, and tests use
   `to_string_lossy()`. A real non-Unicode OS path can therefore be silently rewritten.
9. **Plaintext client secret:** the Rust installer currently copies `MICU_API_KEY` into client
   JSON/TOML. The refactor will keep the secret in process environment or macOS Keychain and will
   not serialize it into either client config.
10. **Output safety is partly strong already:** `cap-std`, same-directory create-new temp files,
    hard-link commit, full decode, and RAII cleanup are present. The path policy and byte writer are
    nevertheless combined in `storage.rs`, and the requested pre/post ancestor checks are not
    exposed as one testable interface.
11. **Input safety is partly strong already:** root and target are canonicalized, then opened
    through a capability directory and snapshotted. Root resolution is repeated and relative input
    semantics remain ambient.
12. **Issue #4 is broader than quote choice:** the reported invalid `\P`/`\U` escapes come from
    hand-authored TOML basic strings. Choosing literal strings happens to fix some paths, but does
    not cover quotes, Unicode, UNC, extended-length paths, comments, idempotency, or AST merge
    preservation. The fix is parser-owned serialization plus exact PathBuf round-trip validation.

## 3. Frozen path semantics after refactor

- Home, startup cwd, executable, data-local directory, config paths, roots, and lock path are
  captured once before the MCP server starts.
- `MICU_SAVE_DIR_ROOT`: exact `~`, `~/...`, and `~\...` expand against captured home; an absolute
  path is accepted; any other relative value resolves against captured home, never cwd. The root is
  created and canonicalized during startup.
- `MICU_SAVE_DIR`: absolute values must be below canonical save root; relative values resolve below
  save root.
- tool `save_dir`: same rule as `MICU_SAVE_DIR`.
- `MICU_INPUT_ROOT`: root syntax follows the save-root rule and is canonicalized at startup.
- input paths with an input root: relative values resolve below input root; absolute values must be
  below it; capability opening rejects symlink escape.
- input paths without an input root: relative values resolve against the cwd captured at server
  startup, preserving compatibility while eliminating later cwd drift.
- only exact `~`, `~/...`, and `~\...` are supported. `~someone` returns a typed error.
- path values remain `Path`/`PathBuf`/`OsStr` until JSON/TOML serialization. Non-Unicode values
  return a typed error instead of lossy conversion.
- lock default remains `home/.cache/micu-image/bigsize.lock` in both Python and Rust so mixed
  reference/native processes serialize the same high-resolution origin queue.

## 4. Deep module interfaces

```text
EnvironmentSnapshot::capture() -> EnvironmentSnapshot
SystemPathSource::capture() -> PathSource
AppPaths::resolve(&EnvironmentSnapshot, PathSource) -> Result<AppPaths, PathError>
Config::from_env(&EnvironmentSnapshot) -> Result<Config, ConfigError>
PathPolicy::new(&AppPaths) -> PathPolicy
AppState::new(config, paths, http, locks) -> Result<AppState, AppError>

InputStore::validate(path, label) -> Result<ValidatedImage, FsError>
OutputStore::resolve(optional_save_dir) -> Result<OutputDirectory, FsError>
OutputStore::save_stream/save_base64(...) -> Result<SavedImage, FsError>

Installer::install(options, AppPaths) -> Result<InstallReport, InstallError>
CodexConfig::merge/verify_round_trip(...)
ClaudeConfig::merge/verify_round_trip(...)
AtomicWriter::replace_verified(...)
BinaryInstaller::install(source, stable_destination) -> Result<PathBuf, InstallError>
```

Callers and tests use these interfaces. Home/cwd/config lookup, lexical normalization,
capability-opening, AST serialization, write-read verification, temp cleanup, and platform
permissions remain implementation details.

## 5. Implemented source structure

The crate remains at repository root so the retained Python reference and existing release commands
continue to work.

```text
src/
  main.rs                     CLI/log/start only
  lib.rs
  app.rs                      production wiring / AppState
  config/
    mod.rs                    non-path runtime Config
    env.rs                    frozen environment and secret loading
    paths.rs                  AppPaths, PathSource, PathPolicy, typed PathError
  domain/
    mod.rs
    size.rs
    routing.rs
  http/
    mod.rs
    client.rs
    download.rs
    response.rs
    retry.rs
  fs/
    mod.rs
    image.rs                  bounded magic/full decode primitives
    input.rs                  InputStore, capability open, upload snapshot, mask policy
    sandbox.rs                save lexical/capability policy and atomic no-clobber commit
    output_store.rs           bounded stream/base64 OutputStore
    response_output.rs        URL/b64 response selection and download adapter
    lock.rs                   cancellation-safe shared lock
  installer/
    mod.rs                    orchestration only
    atomic.rs                 backup + verified atomic replace
    binary.rs                 stable per-user binary install
    codex.rs                  toml_edit merge/reset/round-trip
    claude.rs                 serde_json merge/reset/round-trip
  providers/
  tools/
    mod.rs                    registry and aggregate only
    types.rs                  MCP parameter types
    generate.rs
    edit.rs
    batch.rs
    multi_reference.rs
    server_info.rs
```

`git diff --find-renames` identified the config/domain/fs/http moves as renames (HTTP/domain pure
logic was 97-100% similar). Focused tests ran after each vertical slice, followed by the frozen
STDIO/mock fixtures. The unrelated unknown-tool-argument compatibility fix is isolated in
`2e8b3c7` rather than mixed into the path commit.

## 6. Platform verification

- Cross-platform opaque PathBuf/TOML round-trip tests run everywhere for Windows and POSIX strings.
- `#[cfg(windows)]` tests exercise native Windows prefix/root semantics, drive-root case handling,
  UNC, extended-length prefixes, and `C:\safe` versus `C:\safe2` on the Windows CI runner.
- Unix-only symlink race/escape tests remain gated to Unix. Windows junction tests are gated to
  Windows and skip only when the runner cannot create a junction; that limitation is reported.
- GitHub native jobs remain Ubuntu, macOS arm64/x64, and Windows x86_64. No local macOS result is
  presented as proof that native Windows paths work.
