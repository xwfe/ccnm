"""P57.3：两个样例项目的离线“读改测 + 失败再修复 + 回滚 + 大输出”闭环。

不启动 ccnm、不连网络、不装依赖：只证明样例本身确定、可回滚、可复现，
P62 才把它们放到 Runtime 上交给真实 Agent。步骤见 tests/fixtures/sample-projects/README.md。

    python3 -B docs/research/probes/p57-sample-projects.py
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[3]
SAMPLES = ROOT / "tests/fixtures/sample-projects"
GIT = ["git", "-c", "user.name=p57", "-c", "user.email=p57@example.invalid", "-c", "init.defaultBranch=main"]

COMMANDS = {
    "rust-mini": {
        "test": ["cargo", "test", "--offline", "--locked", "--quiet"],
        "bigout": ["cargo", "run", "--quiet", "--offline", "--locked", "--bin", "bigout", "--", "3"],
    },
    "ts-mini": {
        "test": ["node", "--test", "test/duration.test.ts"],
        "bigout": ["node", "src/bigout.ts", "3"],
    },
}


def run(cmd: list, cwd: Path, env: dict) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, env=env, capture_output=True, timeout=600)


def exercise(name: str, base: Path) -> dict:
    work = base / name
    shutil.copytree(SAMPLES / name, work)
    env = {**os.environ, "CARGO_TARGET_DIR": str(base / f"{name}-target")}
    git = lambda *a: run([*GIT, *a], work, env)  # noqa: E731
    git("init", "-q")
    git("add", "-A")
    git("commit", "-qm", "baseline")
    cmd = COMMANDS[name]
    steps = {}

    def test(label: str) -> None:
        out = run(cmd["test"], work, env)
        text = (out.stdout + out.stderr).decode("utf-8", "replace")
        steps[label] = {"exit": out.returncode}
        if out.returncode != 0:
            # 失败要失败在预期的那一条上，而不是编译错误或环境问题。
            steps[label]["failure_mentions_hours"] = "hours" in text

    test("baseline")
    steps["apply_01"] = git("apply", "task/01-hours-test.patch").returncode
    test("with_failing_test")
    steps["apply_02"] = git("apply", "task/02-hours-fix.patch").returncode
    test("after_fix")
    steps["diff_stat_before_rollback"] = git("diff", "--stat").stdout.decode().strip().splitlines()[-1]
    git("reset", "-q", "--hard")
    git("clean", "-qfdx")
    steps["status_after_rollback"] = git("status", "--porcelain").stdout.decode().strip()
    test("after_rollback")

    big = run(cmd["bigout"], work, env)
    stdout = big.stdout
    steps["bigout"] = {
        "exit": big.returncode,
        "stdout_bytes": len(stdout),
        "sha256": hashlib.sha256(stdout).hexdigest(),
        "markers": {m: stdout.count(m.encode()) for m in ("P57-EARLY-MARKER", "P57-MIDDLE-MARKER", "P57-LATE-MARKER")},
        "valid_utf8": _utf8(stdout),
        "stderr_marker_only_on_stderr": b"P57-STDERR-MARKER" in big.stderr and b"P57-STDERR-MARKER" not in stdout,
    }
    return steps


def _utf8(data: bytes) -> bool:
    try:
        data.decode("utf-8")
        return True
    except UnicodeDecodeError:
        return False


def main() -> None:
    tools = {
        "cargo": subprocess.run(["cargo", "-V"], capture_output=True, text=True).stdout.strip(),
        "node": subprocess.run(["node", "-v"], capture_output=True, text=True).stdout.strip(),
        "git": subprocess.run(["git", "--version"], capture_output=True, text=True).stdout.strip(),
    }
    with tempfile.TemporaryDirectory(prefix="p57-samples-") as tmp:
        results = {name: exercise(name, Path(tmp)) for name in COMMANDS}
    same = results["rust-mini"]["bigout"]["sha256"] == results["ts-mini"]["bigout"]["sha256"]
    ok = all(
        r["baseline"]["exit"] == 0
        and r["with_failing_test"]["exit"] != 0
        and r["after_fix"]["exit"] == 0
        and r["after_rollback"]["exit"] == 0
        and r["status_after_rollback"] == ""
        and r["bigout"]["exit"] == 0
        for r in results.values()
    )
    print(json.dumps({"tools": tools, "results": results, "bigout_identical_across_samples": same, "loop_as_expected": ok},
                     ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
