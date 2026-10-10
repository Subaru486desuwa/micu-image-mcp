# Contributing

`main` is the only maintained Rust implementation. Keep product code, tests and engineering tools in Rust; retain necessary Shell/PowerShell platform bootstrap scripts. Main must contain no Python runtime, packaging, interpreter-driven tests or inline Python/JavaScript workflows. Static historical fixtures may retain their original names.

Preserve the five MCP tools and public schemas, persistent sessions, client configuration and credential/path/image/response limits. Never print or persist API credentials. Offline acceptance uses isolated child processes and local HTTP fixtures; paid image or model validation requires explicit authorization and the corresponding opt-in switches.

Run formatting, workspace Clippy, build and the complete Rust tests as documented in [Rust-only development](docs/rust-only-development.md). Black-box tests must execute the actual Cargo-built server; missing binaries are failures, not skips. Validate OS-specific behavior on native runners.

Only release the root Rust package from main. Version tags must match Cargo.toml and docs/releases/<tag>.md. Assets remain the four native binaries and SHA256SUMS. Do not publish legacy packages or delete historical tags/releases. Manual release workflow runs are build-only.

The Python reference remains separate. Both exact TS histories are archived in main's ancestry before removing ts-port refs; do not restore TS implementation into the current source tree. See [branch policy](docs/branch-release-policy.md) for recovery commands. Temporary codex/* branches are removed only after exact tip retention, unchanged refs and checks for protection/open PRs. Squashed/rebased tips remain until their original history is preserved.
