"""用真正的 nbformat 核对 ccnm 的 notebook 读写（P40）。不在自动测试里跑：本机和 CI
都没装 nbformat。手工跑法：

    python3 -m venv /tmp/nbvenv && /tmp/nbvenv/bin/pip install nbformat
    cargo build
    /tmp/nbvenv/bin/python tests/fixtures/notebook/check_with_nbformat.py

核对四件事，任何一件不成立就以非 0 退出：
  1. analysis.ipynb 符合 nbformat 的 schema；
  2. nbformat 自己重写 analysis.ipynb，结果和文件逐字节一致（样例确实是它的写法）；
  3. 经真实 ccnm 的 apply_patch edit_notebook 做五项编辑后，文件仍符合 schema；
  4. nbformat 重写编辑后的文件，结果和 ccnm 写的逐字节一致。
"""

import base64
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

import nbformat

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tests"))
from mcp_client import McpClient, result_text  # noqa: E402

FIXTURE = Path(__file__).with_name("analysis.ipynb")
EDITS = [
    {"cell_id": "d0f19b3c", "edit_mode": "delete"},
    {"cell_id": "c4e8f7aa", "new_source": "df.describe()"},
    {"cell_id": "5a1c0e2f", "edit_mode": "insert", "cell_type": "code", "new_source": "import numpy as np\n"},
    {"cell_id": "b7d3a901", "new_source": "Now markdown.", "cell_type": "markdown"},
    {"edit_mode": "insert", "cell_type": "markdown", "new_source": "# Top\nline two"},
]


def written_by_nbformat(nb):
    text = nbformat.writes(nb)
    return text if text.endswith("\n") else text + "\n"


def main():
    results = {"nbformat": nbformat.__version__}
    original = FIXTURE.read_text(encoding="utf-8")
    nb = nbformat.reads(original, as_version=4)
    nbformat.validate(nb)
    results["1_fixture_valid"] = True
    results["2_fixture_is_nbformats_own_writing"] = written_by_nbformat(nb) == original

    tmp = Path(tempfile.mkdtemp(prefix="ccnm-nbformat-")).resolve()
    try:
        root = tmp / "project"
        root.mkdir()
        (root / "analysis.ipynb").write_text(original, encoding="utf-8")
        for sub in ("home", "state"):
            (tmp / sub).mkdir()
        config = tmp / "config.toml"
        config.write_text(
            f'this = "runtime"\n\n[nodes.runtime]\n\n[nodes.agent]\nssh = "agent-node.invalid"\n\n'
            f'[workspaces.demo]\nroot = "{root}"\nagent = {{ node = "agent", instance = "claude-main" }}\n'
            f'external_mcp = "coding"\n'
        )
        payload = base64.urlsafe_b64encode(json.dumps(
            {"protocol": 5, "workspace": "demo", "session": "nbformat-check", "mode": "coding"}
        ).encode()).decode().rstrip("=")
        client = McpClient(
            [os.environ.get("CCNM_BIN", str(ROOT / "target" / "debug" / "ccnm")),
             "internal", "mcp-serve", "--payload", payload],
            {"PATH": os.environ["PATH"], "HOME": str(tmp / "home"),
             "XDG_STATE_HOME": str(tmp / "state"), "CCNM_CONFIG": str(config)},
        )
        client.initialize()
        footer = client.call_tool("read_notebook", {"path": "analysis.ipynb"})["content"][-1]["text"]
        version = footer.rsplit("; version ", 1)[1].rstrip("]")
        applied = client.call_tool("apply_patch", {"files": [{
            "op": "edit_notebook", "path": "analysis.ipynb", "version": version, "cells": EDITS}]})
        client.close()
        results["apply_patch"] = result_text(applied).splitlines()[0]
        after = (root / "analysis.ipynb").read_text(encoding="utf-8")
        edited = nbformat.reads(after, as_version=4)
        nbformat.validate(edited)
        results["3_edited_valid"] = True
        results["4_edited_is_nbformats_own_writing"] = written_by_nbformat(edited) == after
    finally:
        shutil.rmtree(tmp)

    print(json.dumps(results, ensure_ascii=False, indent=2))
    checks = [v for k, v in results.items() if k[0].isdigit()]
    sys.exit(0 if all(checks) else 1)


if __name__ == "__main__":
    main()
