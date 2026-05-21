"""单元测试 conftest：在 import server 之前把沙箱根设到 /tmp，避免污染 ~/Pictures。

server.py 顶层会读 MICU_SAVE_DIR_ROOT / MICU_SAVE_DIR 构造 module 级常量
（_SAVE_ROOT / DEFAULT_SAVE_DIR），import 之后再改 env 不会重读。
所以这里在 sys.path / env 设置完成后才让 pytest 收集 server 符号。
"""
from __future__ import annotations

import os
import sys
import tempfile
from pathlib import Path


_SANDBOX_ROOT = Path(tempfile.gettempdir()) / "micu-image-mcp-unit-test"
_SANDBOX_ROOT.mkdir(parents=True, exist_ok=True)

os.environ["MICU_SAVE_DIR_ROOT"] = str(_SANDBOX_ROOT)
os.environ["MICU_SAVE_DIR"] = str(_SANDBOX_ROOT)
# 不读 shell 代理避免本机配置干扰
os.environ.setdefault("MICU_USE_SHELL_PROXY", "0")

# 让测试能 import server（仓库根目录加入 sys.path）
_REPO_ROOT = Path(__file__).resolve().parent.parent.parent
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))
