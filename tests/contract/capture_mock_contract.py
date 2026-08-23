from __future__ import annotations

import argparse
import json
import shlex
import sys
from pathlib import Path

from tests.contract.contract_cases import REPO_ROOT, case_names, run_case


DEFAULT_OUTPUT = Path(__file__).resolve().parent / "fixtures" / "python" / "mock-cases.json"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--server-command",
        default=f"{shlex.quote(sys.executable)} {shlex.quote(str(REPO_ROOT / 'server.py'))}",
    )
    parser.add_argument("--case", choices=case_names(), action="append")
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    command = shlex.split(args.server_command)
    selected = args.case or case_names()
    payload = {}
    for name in selected:
        print(f"[case] {name}", file=sys.stderr, flush=True)
        payload[name] = run_case(command, name)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    try:
        display_path = args.output.relative_to(REPO_ROOT)
    except ValueError:
        display_path = args.output
    print(display_path)


if __name__ == "__main__":
    main()
