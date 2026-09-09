# Contributing

## Mainline and legacy runtimes

`main` is the Rust mainline. Send new functionality, fixes to the supported runtime,
and release changes to `main`. Do not recreate a long-lived `rust-port` branch.

`ts-port` and `python-reference` preserve the legacy TypeScript and Python versions.
Keep those rollback/reference branches separate from the Rust release train. Legacy
compatibility fixes may be reviewed during the transition; do not merge a whole old
runtime implementation into `main` merely to synchronize it. The retired Rust port
has a documented history-only archival merge; its old implementation is not adopted.

Temporary feature branches are welcome. Same-repository `codex/*` branches merged
into `main` are removed only when their exact tip is retained in main's ancestry and
they have no newer commits, protection rules, or other open PRs depending on them.
After squash/rebase, an original tip that is not an ancestor stays for manual review.
Other feature branches should be removed after merge by their maintainer.

## Releases

Only release the root Rust package from commits on `main`. A version tag must match
`Cargo.toml`, and its notes must exist at `docs/releases/<tag>.md`. Release attachments
are the four supported native Rust binaries and `SHA256SUMS`; do not attach legacy
Python or TypeScript packages. Manual workflow runs on `main` are build-only.

Do not delete historical releases or tags as part of the runtime transition. Keep
Python differential and lock compatibility tests until equivalent standalone Rust
coverage and legacy rollback preservation have been reviewed.

See [the branch and release policy](docs/branch-release-policy.md) for the exact
migration checkpoints, phased removal plan, and rollback commands.
