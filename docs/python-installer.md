# Python reference installer: PEP 668 and virtual environments

This is the legacy/source-tree Python installer, not the supported Rust release
installer. New Rust installations should use the Rust binary's `install` command.
Neither the Rust release workflow nor its artifacts are changed by this fix.

## Run the correct installer for your branch

On `main`, explicitly select the Python rollback runtime:

```sh
python install.py --runtime python
```

On the Python-only `python-reference` branch, there is no `--runtime` option:

```sh
python install.py
```

Both support `--yes` with `MICU_API_KEY` and optional `MICU_SAVE_DIR` environment
variables. Do not put real keys in commands, issue reports, or screenshots.

## Environment selection

When the interpreter is not already in a virtual environment and its standard
library contains the `EXTERNALLY-MANAGED` marker, the installer creates or reuses
`<repository>/.venv`. It then restarts itself using that environment's Python.
Dependency installation, `/v1/models` key-group validation, the startup smoke test,
and the command written to Claude/Codex therefore use the same interpreter.
The system interpreter does not need pip or httpx for this bootstrap.

Existing venv/virtualenv environments are detected from the interpreter's prefixes;
Conda is recognized by `conda-meta` under its actual prefix. Stale `VIRTUAL_ENV` or
`CONDA_PREFIX` shell variables do not establish which interpreter is running.
Without an explicit destination, an active environment or an unmanaged system
interpreter keeps the previous installation behavior.

To select a different environment, including on an unmanaged interpreter or while
another venv is active:

```sh
python install.py --venv-dir "$HOME/venvs/micu"
```

Relative paths are relative to the current working directory and are made absolute
before launch/configuration. Paths containing spaces are supported. No shell
activation is required. Do not move/delete the chosen environment or this source
checkout while a client configuration still references them.

## Creating and recovering environments

Creation prefers `python -m venv`. If unavailable, an installed `uv` is tried with
`venv --seed` and an explicit `--python` matching the installer interpreter. A
missing pip inside a valid target is bootstrapped with `ensurepip`, then an installed
`uv` if necessary. `--mirror` / `--pypi-index` also apply to uv package downloads.
The installer does not install uv or modify OS packages on your behalf.

Existing targets must contain `pyvenv.cfg`, run Python 3.10 or later, and report the
expected virtual-environment prefix. An invalid target is **never recursively
deleted or automatically replaced**. Repair it manually or choose a new
`--venv-dir`; partially created environments are preserved as well. On
Debian/Ubuntu, a missing stdlib venv/ensurepip commonly requires the distribution's
`python3-venv` package. Changing a package index cannot fix a PEP 668 restriction.

The risky override is opt-in only:

```sh
python install.py --break-system-packages
```

This forwards the flag to both pip installation paths and may damage
OS-managed Python packages. It cannot be combined with `--venv-dir` and is not the
recommended workaround. The normal PEP 668 path never enables it automatically.

`--help`, `--reset`, and `--runtime rust` on `main` do not create a Python environment
or install Python server dependencies. Reset removes the managed client entries,
not the environment or installed packages. Use the Python command from the old
client configuration when uninstalling packages; do not assume it was system Python.

## Regression coverage

Run `python -m pytest -q tests/unit/test_installer_env.py`. Tests cover marker and
prefix detection, stale activation variables, destination precedence, absolute
paths, invalid-directory preservation, creation/pip fallback command routing,
explicit overrides, reset/help, and the Rust bypass where available.

The integration regression uses a real temporary venv, interpreter re-execution,
client configuration writes, and stdio subprocess. Its pip, httpx, and MCP server
are offline test doubles: it verifies interpreter routing, not real dependency
resolution or the remote image API. No user configuration or real API credentials
are used. Native platform CI and live-service validation are separate checks.

This addresses issue #5, reported by @tingfeng347. Reference specifications:

- [Externally Managed Environments](https://packaging.python.org/en/latest/specifications/externally-managed-environments/)
- [Python venv](https://docs.python.org/3/library/venv.html)
- [uv command reference](https://docs.astral.sh/uv/reference/cli/)
