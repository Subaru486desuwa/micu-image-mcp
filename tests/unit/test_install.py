from __future__ import annotations

import install


def test_installer_ignores_legacy_grok_environment(monkeypatch, tmp_path):
    monkeypatch.setenv("MICU_API_KEY", "sk-image2-test")
    monkeypatch.setenv("MICU_GROK_API_KEY", "sk-grok-test")
    monkeypatch.setenv("MICU_SAVE_DIR", str(tmp_path))
    monkeypatch.setattr(install, "_validate_key_group", lambda **_kwargs: True)

    env, _save_dir, _save_root = install.collect_config(
        non_interactive=True,
        baseurl=install.DEFAULT_BASEURL,
    )

    assert env["MICU_API_KEY"] == "sk-image2-test"
    assert "MICU_GROK_API_KEY" not in env
    assert "XAI_MODEL" not in env
