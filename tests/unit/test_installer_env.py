"""Offline regressions for issue #5; no user config or real API keys are used."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from types import SimpleNamespace
from unittest.mock import Mock
import venv

import pytest

import install
import installer_env as bootstrap


def system_python(monkeypatch, tmp_path):
    monkeypatch.setattr(bootstrap.sys, "prefix", str(tmp_path / "system"))
    monkeypatch.setattr(bootstrap.sys, "base_prefix", str(tmp_path / "system"))
    monkeypatch.delattr(bootstrap.sys, "real_prefix", raising=False)
    monkeypatch.setattr(bootstrap.sysconfig, "get_path", lambda *a: str(tmp_path))


def test_marker_and_stale_activation_variables(monkeypatch, tmp_path):
    system_python(monkeypatch, tmp_path)
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "unrelated"))
    monkeypatch.setenv("CONDA_PREFIX", str(tmp_path / "unrelated"))
    assert not bootstrap.externally_managed()
    (tmp_path / "EXTERNALLY-MANAGED").write_text("[externally-managed]\n")
    assert bootstrap.externally_managed()


def test_marker_directory_is_not_a_marker_file(monkeypatch, tmp_path):
    system_python(monkeypatch, tmp_path)
    (tmp_path / "EXTERNALLY-MANAGED").mkdir()
    assert not bootstrap.externally_managed()


@pytest.mark.parametrize("kind", ["venv", "virtualenv", "conda"])
def test_actual_environments_ignore_marker(monkeypatch, tmp_path, kind):
    system_python(monkeypatch, tmp_path)
    (tmp_path / "EXTERNALLY-MANAGED").touch()
    if kind == "venv":
        monkeypatch.setattr(bootstrap.sys, "prefix", str(tmp_path / "active"))
    elif kind == "virtualenv":
        monkeypatch.setattr(bootstrap.sys, "real_prefix", "/base", raising=False)
    else:
        (Path(bootstrap.sys.prefix) / "conda-meta").mkdir(parents=True)
    assert bootstrap.in_virtual_environment()
    assert not bootstrap.externally_managed()


@pytest.mark.parametrize("managed,explicit", [(True, None), (False, "custom env"), (True, "custom env")])
def test_selects_absolute_target_and_reexecs(monkeypatch, tmp_path, managed, explicit):
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr(bootstrap, "externally_managed", lambda: managed)
    monkeypatch.setattr(bootstrap, "in_virtual_environment", lambda: False)
    create = Mock()
    ensure = Mock()
    execute = Mock()
    monkeypatch.setattr(bootstrap, "_create_venv", create)
    monkeypatch.setattr(bootstrap, "_validate_venv", bootstrap.venv_python)
    monkeypatch.setattr(bootstrap, "_ensure_pip", ensure)
    monkeypatch.setattr(bootstrap, "_exec_installer", execute)
    monkeypatch.setattr(sys, "argv", ["install.py", "--yes"])
    monkeypatch.setenv("MICU_API_KEY", "sk-test-fixture")
    monkeypatch.setenv("PYTHONHOME", "/stale-python-home")
    bootstrap.prepare_python(tmp_path, venv_dir=explicit, mirror_url="https://index.invalid/simple")
    directory = (tmp_path / (explicit or ".venv")).resolve()
    python = bootstrap.venv_python(directory)
    create.assert_called_once_with(directory, "https://index.invalid/simple")
    ensure.assert_called_once_with(python, "https://index.invalid/simple")
    executable, argv, env = execute.call_args.args
    assert executable == str(python)
    assert argv == [str(python), str(tmp_path / "install.py"), "--yes"]
    assert "sk-test-fixture" not in repr(argv)
    assert env["MICU_API_KEY"] == "sk-test-fixture"
    assert "PYTHONHOME" not in env
    assert env["VIRTUAL_ENV"] == str(directory)


def test_unmanaged_default_does_nothing(monkeypatch, tmp_path):
    monkeypatch.setattr(bootstrap, "externally_managed", lambda: False)
    create = Mock(side_effect=AssertionError("must not create"))
    monkeypatch.setattr(bootstrap, "_create_venv", create)
    bootstrap.prepare_python(tmp_path)
    create.assert_not_called()


def test_explicit_target_can_switch_from_active_venv(monkeypatch, tmp_path):
    monkeypatch.setattr(bootstrap, "in_virtual_environment", lambda: True)
    monkeypatch.setattr(bootstrap.sys, "prefix", str(tmp_path / "old"))
    monkeypatch.setattr(bootstrap, "_create_venv", Mock())
    monkeypatch.setattr(bootstrap, "_validate_venv", bootstrap.venv_python)
    monkeypatch.setattr(bootstrap, "_ensure_pip", Mock())
    execute = Mock()
    monkeypatch.setattr(bootstrap, "_exec_installer", execute)
    bootstrap.prepare_python(tmp_path, venv_dir=str(tmp_path / "new"))
    assert execute.call_args.args[0] == str(bootstrap.venv_python(tmp_path / "new"))


def test_already_in_explicit_target_does_not_loop(monkeypatch, tmp_path):
    monkeypatch.setattr(bootstrap, "in_virtual_environment", lambda: True)
    monkeypatch.setattr(bootstrap.sys, "prefix", str(tmp_path))
    execute = Mock(side_effect=AssertionError("re-exec loop"))
    monkeypatch.setattr(bootstrap, "_exec_installer", execute)
    bootstrap.prepare_python(tmp_path, venv_dir=str(tmp_path))
    execute.assert_not_called()


def test_break_system_is_explicit_and_conflicts_with_venv(monkeypatch, tmp_path, capsys):
    create = Mock(side_effect=AssertionError("must not create"))
    monkeypatch.setattr(bootstrap, "_create_venv", create)
    bootstrap.prepare_python(tmp_path, break_system_packages=True)
    assert "--break-system-packages" in capsys.readouterr().out
    with pytest.raises(SystemExit, match="不能同时使用"):
        bootstrap.prepare_python(tmp_path, venv_dir="target", break_system_packages=True)
    create.assert_not_called()


def test_invalid_existing_directory_is_preserved(monkeypatch, tmp_path):
    marker = tmp_path / "important.txt"
    marker.write_text("keep this")
    with pytest.raises(SystemExit, match="已有文件保持不变"):
        bootstrap.prepare_python(tmp_path, venv_dir=str(tmp_path))
    assert marker.read_text() == "keep this"


@pytest.mark.parametrize("payload", [
    [[3, 9], "TARGET", "/base"],
    [[3, 13], "/base", "/base"],
    [[3, 13], "/wrong", "/base"],
    None, [], {"bad": "shape"}, "not metadata",
])
def test_rejects_bad_interpreter_probe(monkeypatch, tmp_path, payload):
    (tmp_path / "pyvenv.cfg").touch()
    encoded = json.dumps(payload).replace('"TARGET"', json.dumps(str(tmp_path)))
    monkeypatch.setattr(bootstrap.subprocess, "run", Mock(return_value=SimpleNamespace(returncode=0, stdout=encoded)))
    with pytest.raises(SystemExit, match="不是可用"):
        bootstrap._validate_venv(tmp_path)


def test_real_venv_keeps_its_executable_path(tmp_path):
    target = tmp_path / "env with spaces"
    venv.EnvBuilder(with_pip=False).create(target)
    python = bootstrap._validate_venv(target.resolve())
    assert python == bootstrap.venv_python(target)
    metadata = subprocess.check_output([str(python), "-I", "-c", "import sys;print(sys.prefix)"], text=True)
    assert Path(metadata.strip()).resolve() == target.resolve()


def test_stdlib_creation_is_preferred(monkeypatch, tmp_path):
    run = Mock(return_value=True)
    monkeypatch.setattr(bootstrap, "_succeeds", run)
    monkeypatch.setattr(bootstrap.shutil, "which", Mock(side_effect=AssertionError("no uv needed")))
    target = tmp_path / "new"
    bootstrap._create_venv(target, None)
    run.assert_called_once_with([sys.executable, "-m", "venv", str(target)])


def test_uv_fallback_is_pinned_non_destructive_and_uses_mirror(monkeypatch, tmp_path):
    run = Mock(side_effect=[False, True])
    monkeypatch.setattr(bootstrap, "_succeeds", run)
    monkeypatch.setattr(bootstrap.shutil, "which", lambda name: "/tools/uv")
    monkeypatch.setenv("UV_VENV_CLEAR", "true")
    bootstrap._create_venv(tmp_path / "new", "https://index.invalid/simple")
    command = run.call_args.args[0]
    assert command[:5] == ["/tools/uv", "venv", "--seed", "--allow-existing", "--no-config"]
    assert command[command.index("--python") + 1] == sys.executable
    assert command[-2:] == ["--index-url", "https://index.invalid/simple"]
    assert run.call_args.kwargs["env"]["UV_VENV_CLEAR"] == "false"


def test_failed_creation_preserves_partial_directory(monkeypatch, tmp_path):
    target = tmp_path / "new"
    monkeypatch.setattr(bootstrap, "_succeeds", lambda *a, **kw: False)
    monkeypatch.setattr(bootstrap.shutil, "which", lambda name: None)
    with pytest.raises(SystemExit, match="python3-venv"):
        bootstrap._create_venv(target, None)
    assert target.is_dir()


def test_creation_never_overwrites_existing_directory(tmp_path):
    (tmp_path / "keep").touch()
    with pytest.raises(SystemExit, match="无法创建"):
        bootstrap._create_venv(tmp_path, None)
    assert (tmp_path / "keep").exists()


@pytest.mark.parametrize("results, count", [([True], 1), ([False, True], 2), ([False, False, True], 3)])
def test_pip_bootstrap_only_targets_venv(monkeypatch, tmp_path, results, count):
    run = Mock(side_effect=results)
    monkeypatch.setattr(bootstrap, "_succeeds", run)
    monkeypatch.setattr(bootstrap.shutil, "which", lambda name: "/tools/uv")
    python = bootstrap.venv_python(tmp_path)
    bootstrap._ensure_pip(python, "https://index.invalid/simple")
    assert run.call_count == count
    for call in run.call_args_list:
        assert str(python) in call.args[0]
    if count == 3:
        assert run.call_args.args[0][-3:] == ["pip", "--index-url", "https://index.invalid/simple"]


def test_missing_pip_fails_without_touching_system(monkeypatch, tmp_path):
    monkeypatch.setattr(bootstrap, "_succeeds", lambda *a, **kw: False)
    monkeypatch.setattr(bootstrap.shutil, "which", lambda name: None)
    with pytest.raises(SystemExit, match="未向系统 Python"):
        bootstrap._ensure_pip(bootstrap.venv_python(tmp_path), None)


@pytest.mark.parametrize("failure", [OSError("missing"), subprocess.TimeoutExpired("python", 120)])
def test_probe_failures_are_actionable(monkeypatch, failure):
    monkeypatch.setattr(bootstrap.subprocess, "run", Mock(side_effect=failure))
    assert not bootstrap._succeeds(["missing"])


@pytest.mark.parametrize("override", [False, True])
def test_editable_and_fallback_share_interpreter_and_options(monkeypatch, tmp_path, override):
    run = Mock(side_effect=[SimpleNamespace(returncode=1), SimpleNamespace(returncode=0)])
    monkeypatch.setattr(install.subprocess, "run", run)
    monkeypatch.setattr(install.sys, "executable", str(bootstrap.venv_python(tmp_path)))
    install.install_deps(tmp_path, "https://index.invalid/simple", break_system_packages=override)
    for call in run.call_args_list:
        command = call.args[0]
        assert command[:4] == [install.sys.executable, "-m", "pip", "install"]
        assert ("--break-system-packages" in command) is override
        assert "https://index.invalid/simple" in command


def test_cli_environment_flags_are_mutually_exclusive(monkeypatch):
    monkeypatch.setattr(sys, "argv", ["install.py", "--venv-dir", "a", "--break-system-packages"])
    with pytest.raises(SystemExit) as exc:
        install.parse_args()
    assert exc.value.code == 2


@pytest.mark.parametrize("flag", ["--help", "--reset"])
def test_help_and_reset_never_bootstrap(monkeypatch, flag):
    monkeypatch.setattr(sys, "argv", ["install.py", flag])
    prepare = Mock(side_effect=AssertionError("must not bootstrap"))
    monkeypatch.setattr(install, "prepare_python", prepare)
    monkeypatch.setattr(install, "do_reset", Mock())
    if flag == "--help":
        with pytest.raises(SystemExit) as exc:
            install.main()
        assert exc.value.code == 0
    else:
        install.main()
    prepare.assert_not_called()


def test_prepare_runs_before_pip_and_config(monkeypatch, tmp_path):
    monkeypatch.setattr(sys, "argv", ["install.py", "--yes"])
    prepare = Mock(side_effect=SystemExit("bootstrap boundary"))
    monkeypatch.setattr(install, "prepare_python", prepare)
    monkeypatch.setattr(install, "check_python", Mock())
    monkeypatch.setattr(install, "check_running_clients", Mock())
    monkeypatch.setattr(install, "check_pip", Mock(side_effect=AssertionError("too early")))
    monkeypatch.setattr(install, "collect_config", Mock(side_effect=AssertionError("too early")))
    with pytest.raises(SystemExit, match="bootstrap boundary"):
        install.main()
    prepare.assert_called_once()


@pytest.mark.skipif(not hasattr(install, "RuntimeCommand"), reason="Python-only reference has no Rust CLI")
def test_rust_never_bootstraps_or_installs_python(monkeypatch, tmp_path):
    monkeypatch.setattr(sys, "argv", ["install.py", "--runtime", "rust", "--yes", "--no-smoke", "--no-claude", "--no-codex"])
    for name in ("prepare_python", "check_pip", "install_deps"):
        monkeypatch.setattr(install, name, Mock(side_effect=AssertionError(name)))
    monkeypatch.setattr(install, "check_python", Mock())
    monkeypatch.setattr(install, "check_running_clients", Mock())
    monkeypatch.setattr(install, "resolve_runtime_command", lambda *a: install.RuntimeCommand("rust", "/fake/rust", []))
    monkeypatch.setattr(install, "collect_config", lambda *a: ({}, "", ""))
    monkeypatch.setattr(install, "summary", Mock())
    install.main()
    assert not (tmp_path / ".venv").exists()


def test_real_reexec_routes_pip_http_and_stdio_to_one_venv(tmp_path):
    """Real venv/exec/config I/O; pip, httpx and MCP server are offline doubles."""
    repo = tmp_path / "repo with spaces"
    repo.mkdir()
    for name in ("install.py", "installer_env.py"):
        shutil.copyfile(Path(install.__file__).parent / name, repo / name)
    target = repo / ".venv"
    venv.EnvBuilder(with_pip=False).create(target)
    python = bootstrap.venv_python(target)
    site = Path(subprocess.check_output(
        [str(python), "-I", "-c", "import sysconfig;print(sysconfig.get_path('purelib'))"], text=True,
    ).strip())
    pip = site / "pip"
    pip.mkdir()
    (pip / "__init__.py").touch()
    logger = "import os,sys,json\nfrom pathlib import Path\nwith open(os.environ['TEST_LOG'], 'a') as f: f.write(json.dumps([KIND,sys.executable,sys.prefix])+ '\\n')\n"
    (pip / "__main__.py").write_text("KIND='pip'\n" + logger + "print('pip fixture')\n")
    (site / "httpx.py").write_text(
        "KIND='http'\n" + logger +
        "class Response:\n status_code=200\n def json(self): return {'data':[{'id':'gpt-image-2'}]}\n"
        "def get(url, **kw):\n assert kw['trust_env'] is False\n return Response()\n"
    )
    (repo / "server.py").write_text(
        "KIND='server'\n" + logger +
        "for line in sys.stdin:\n"
        " obj=json.loads(line)\n"
        " if obj.get('id')==1: print(json.dumps({'id':1,'result':{'protocolVersion':'2024-11-05'}}),flush=True)\n"
        " if obj.get('id')==2: print(json.dumps({'id':2,'result':{'tools':[{'name':n} for n in "
        + repr(sorted(install._EXPECTED_TOOLS)) + "]}}),flush=True)\n"
    )
    home = tmp_path / "home"
    home.mkdir()
    (home / ".claude.json").write_text(json.dumps({"mcpServers": {"other": {"command": "keep"}}}))
    (home / ".codex").mkdir()
    (home / ".codex/config.toml").write_text('[mcp_servers.other]\ncommand = "keep"\n')
    log = tmp_path / "calls.jsonl"
    env = {**os.environ, "HOME": str(home), "USERPROFILE": str(home),
           "MICU_API_KEY": "sk-test-fixture", "MICU_SAVE_DIR": str(tmp_path / "images"),
           "TEST_LOG": str(log), "VIRTUAL_ENV": str(tmp_path / "stale")}
    env.pop("PYTHONHOME", None)
    env.pop("PYTHONPATH", None)
    # Simulate only the OS marker; every operation after selection uses real exec.
    code = "import sys,installer_env,install;installer_env.externally_managed=lambda:True;sys.argv=['install.py','--yes'];install.main()"
    result = subprocess.run([sys.executable, "-c", code], cwd=repo, env=env, capture_output=True, text=True, timeout=60)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "tools/list OK" in result.stdout
    assert "httpx 不可用" not in result.stdout
    rows = [json.loads(line) for line in log.read_text().splitlines()]
    assert {row[0] for row in rows} == {"pip", "http", "server"}
    assert {Path(row[1]) for row in rows} == {python}
    assert {Path(row[2]).resolve() for row in rows} == {target.resolve()}
    claude = json.loads((home / ".claude.json").read_text())
    assert claude["mcpServers"]["other"]["command"] == "keep"
    assert claude["mcpServers"]["micu-image"]["command"] == str(python)
    codex = (home / ".codex/config.toml").read_text()
    assert 'command = "keep"' in codex
    assert json.dumps(str(python)) in codex


@pytest.mark.parametrize("returncode", [0, 7])
def test_windows_restart_quotes_arguments_and_propagates_status(monkeypatch, returncode):
    monkeypatch.setattr(bootstrap.sys, "platform", "win32")
    command = ["C:/env with spaces/python.exe", "C:/repo with spaces/install.py", "--yes"]
    env = {"VIRTUAL_ENV": "C:/env with spaces"}
    run = Mock(return_value=SimpleNamespace(returncode=returncode))
    monkeypatch.setattr(bootstrap.subprocess, "run", run)
    with pytest.raises(SystemExit) as exc:
        bootstrap._exec_installer(command[0], command, env)
    assert exc.value.code == returncode
    run.assert_called_once_with(command, env=env, check=False)


def test_posix_restart_replaces_current_process(monkeypatch):
    monkeypatch.setattr(bootstrap.sys, "platform", "linux")
    command = ["/env with spaces/bin/python", "/repo with spaces/install.py"]
    env = {"VIRTUAL_ENV": "/env with spaces"}
    execute = Mock()
    monkeypatch.setattr(bootstrap.os, "execve", execute)
    bootstrap._exec_installer(command[0], command, env)
    execute.assert_called_once_with(command[0], command, env)
