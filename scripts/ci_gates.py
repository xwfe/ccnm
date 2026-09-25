"""Rust 之外的门禁：计划检查、协议检查和 Python 全套测试（C51-03，P53）。

从仓库根执行，本地和 CI 是同一条命令：

    python3 scripts/ci_gates.py

它先 `cargo build -p ccnm-cli`，把刚构建出的那个 ccnm 交给 CCNM_BIN，再跑测试。
这样中立客户端测试既不会因为找不到二进制整类跳过（unittest 照样报 OK），也不会
拿缓存恢复出来的旧二进制充数。机器上没有 cargo、二进制已经有了（比如在 Runtime
真机上），用 --bin 指定。

**任何 skip 都算失败**：CI 机器上每条测试都该能跑。确实只能在某些机器上跑的，
写进 ALLOWED_SKIPS（测试 id、原因、为什么那台机器不满足）；没写进去的 skip 让
门禁变红，这正是它存在的理由。
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]

# 测试 id -> 为什么它在 CI 机器上可以跳过。空着是有意的：macOS 与
# ubuntu-24.04 的 runner 账号不是 root、有 sudo/admin，p3 与 p12 里那两处条件
# 跳过都不会触发；其余的 skip 只剩"没有二进制"，而这里总会给一个。
ALLOWED_SKIPS: dict[str, str] = {}


def annotate(title: str, lines: list[str]) -> None:
    """在 GitHub Actions 里多打一条注解：匿名也能从提交页看到，读日志却要登录。"""
    if os.environ.get("GITHUB_ACTIONS") != "true":
        return
    text = "\n".join(lines)
    text = text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
    print(f"::error title={title}::{text}", flush=True)


def build() -> Path:
    """构建 CLI，返回 cargo 报告的那个可执行文件——不猜 target 目录在哪。"""
    built = subprocess.run(
        ["cargo", "build", "-p", "ccnm-cli", "--message-format=json-render-diagnostics"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        text=True,
        check=False,
    )
    if built.returncode != 0:
        raise SystemExit("cargo build -p ccnm-cli 失败，见上面的编译输出")
    for line in built.stdout.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == "ccnm"
            and message.get("executable")
        ):
            return Path(message["executable"])
    raise SystemExit("cargo build 成功了，但没有报告 ccnm 可执行文件")


def identify(binary: Path) -> str:
    """确认它真是 ccnm：CCNM_BIN 指错了，测试会用错的程序跑出一堆看不懂的失败。"""
    try:
        said = subprocess.run(
            [str(binary), "--version"], capture_output=True, text=True, check=False, timeout=30
        ).stdout.strip()
    except OSError as error:
        raise SystemExit(f"{binary} 运行不了：{error}")
    if not said.startswith("ccnm "):
        raise SystemExit(f"{binary} --version 说的是 {said!r}，不是 ccnm")
    return said


def run_checks() -> list[str]:
    problems = []
    for script in ("check_plan.py", "check_protocol.py"):
        print(f"== {script}", flush=True)
        done = subprocess.run([sys.executable, "-B", str(ROOT / "scripts" / script)], cwd=ROOT, check=False)
        if done.returncode != 0:
            problems.append(f"{script} 退出码 {done.returncode}")
    return problems


def run_tests(tests: Path) -> list[str]:
    print(f"== python -m unittest discover -s {tests}", flush=True)
    suite = unittest.defaultTestLoader.discover(start_dir=str(tests), top_level_dir=str(tests))
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    problems = []
    if result.testsRun == 0:
        problems.append("一条测试都没跑")
    problems += [f"失败 {test.id()}" for test, _ in result.failures]
    problems += [f"出错 {test.id()}" for test, _ in result.errors]
    problems += [f"意外通过 {test.id()}" for test in result.unexpectedSuccesses]
    problems += [
        f"跳过 {test.id()}：{reason}"
        for test, reason in result.skipped
        if test.id() not in ALLOWED_SKIPS
    ]
    allowed = [test.id() for test, _ in result.skipped if test.id() in ALLOWED_SKIPS]
    print(
        f"python: {result.testsRun} ran, {len(result.skipped)} skipped"
        f" ({len(allowed)} allowed), {len(result.failures)} failed, {len(result.errors)} errors",
        flush=True,
    )
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin", type=Path, help="已经构建好的 ccnm；不给就 cargo build")
    parser.add_argument("--tests", type=Path, default=ROOT / "tests", help="测试目录（默认 tests/）")
    args = parser.parse_args()

    sys.dont_write_bytecode = True
    binary = (args.bin or build()).resolve()
    print(f"== CCNM_BIN={binary} ({identify(binary)})", flush=True)
    # 在导入任何测试模块之前设：各测试文件在导入时就读它。
    os.environ["CCNM_BIN"] = str(binary)

    problems = run_checks() + run_tests(args.tests.resolve())
    if problems:
        print("\n门禁没过：", file=sys.stderr)
        for problem in problems:
            print(f"- {problem}", file=sys.stderr)
        annotate("scripts/ci_gates.py failed", problems[:30])
        return 1
    print("门禁通过：计划、协议与 Python 全套。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
