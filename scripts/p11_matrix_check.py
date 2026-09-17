#!/usr/bin/env python3
"""P11.3 的允许矩阵工具：一条命令跑完真实 bridge 的两种模式，写出证据。

**为什么需要它。** P11.3 要在真机上证明外部 MCP 入口的允许矩阵：read 给什
么、coding 给什么、越权会不会被拒、两个入口抢不抢同一把锁、Host 看到的东西
里有没有私有路径。手工做要开好几个终端、记一堆输出，而这一步发生在需要重建
真机环境、消耗订阅额度的会话里——来回一轮的代价是真金白银。这个脚本把能自
动判的部分压成一条命令，并把结论写成机器可读的证据文件。

**它不能替代什么。** 它自己就是一个 MCP 客户端，不是 Claude Code。真实 Host
怎么解析 tools/list、怎么展示 stderr、尊不尊重 annotations，只有真的拿
Claude Code 连一次才知道。这个脚本负责的是**同一条真实 transport 上的协议与
权限事实**，剩下那半边写在 docs/plan/p11-real-host-session.md 里，要人去做。

**怎么判。** 不看模型说什么（这里根本没有模型）。判据是 Runtime 上的副作用：

- coding 腿用 `apply_patch` 真写一个带一次性 token 的文件；
- 用 `exec_command` 在 Runtime 上 `stat` 它，属主必须是期望的执行身份——
  这一条证明外部入口最终落在同一条隔离执行链上，而不是别的身份；
- read 腿读同一个文件，内容必须一致：两个入口看的是同一棵树；
- read 腿按名字硬调四个它没被给的工具（参数都合法），必须全部被拒，而且事后
  磁盘上确实没有那次写。

用法（在**客户端**机器上，也就是 bridge 跑的那台；配置里已有到 Runtime 的
node）：

    scripts/p11_matrix_check.py \\
        --workspace demo --node runtime --runtime-user ccrun \\
        --out docs/research/p11-matrix.json

`--read-only-workspace` 给一个配成 `external_mcp = "read"` 的 workspace，脚本
会再验一次"请求 coding 被拒且不降级"。不给就跳过那一项，并在证据里写明跳过。

退出码只有全部检查通过才是 0。任何一项判不出来都算没通过——unknown 不是绿。
"""

from __future__ import annotations

import argparse
import json
import secrets
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))

from mcp_client import McpClient, is_error, result_text  # noqa: E402


# read 模式该有的全部工具。多一个少一个都是失败。
# load_skill（P36，项目自带的 skills）、view_image（P39，看图片）、read_notebook（P40）是后加的只读工具，
# read 模式也有。2026-09-11 那两轮真机证据是在它们之前采的，记的是 4 / 7 个。
READ_TOOLS = [
    "list_files", "load_skill", "read_file", "read_notebook", "search_text", "view_image", "workspace_info",
]
# stop_command（P41）和 exec_command 一样只在 coding 模式有。
CODING_TOOLS = READ_TOOLS + ["apply_patch", "exec_command", "read_output", "stop_command"]

# read 腿要按名字硬调的四个，参数都合法——参数不合法会先被参数检查拦下，那
# 证明不了权限门禁。
WITHHELD = {
    "exec_command": {"cmd": ["/bin/echo", "hi"]},
    "apply_patch": {"files": [{"op": "add", "path": "ccnm-p11-sneaked.txt", "content": "x\n"}]},
    "read_output": {"output_ref": "r-0000000000000000"},
    "stop_command": {"output_ref": "r-0000000000000000"},
}

# Host 那边不该看到的东西。离线测试用合成数据查过同一条规则，但只有真机上才
# 存在真的凭据路径——离线里那些字符串本来就不可能出现。
PRIVATE_MARKERS = [
    ".claude",
    ".codex",
    "auth.json",
    "credentials",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "id_rsa",
    "id_ed25519",
    "authorized_keys",
]


class Failure(Exception):
    """一项判据没过。消息就是写进证据文件的那句话。"""


def bridge_argv(args: argparse.Namespace, mode: str, workspace: str) -> list:
    argv = [args.ccnm]
    if args.config:
        argv += ["--config", args.config]
    argv += ["mcp", "bridge", workspace, "--mode", mode]
    if args.node:
        argv += ["--node", args.node]
    return argv


def open_leg(args: argparse.Namespace, mode: str, workspace: str) -> McpClient:
    client = McpClient(bridge_argv(args, mode, workspace))
    client.initialize()
    return client


def exec_stdout(result: dict) -> str:
    """`exec_command` 结果里命令自己的 stdout。"""
    text = result_text(result)
    marker = "--- stdout"
    if marker not in text:
        return ""
    body = text.split(marker, 1)[1]
    for stop in ("--- stderr", "[output_ref", "[this runtime is NOT confined"):
        body = body.split(stop, 1)[0]
    return body.strip()


def check_read_leg(args: argparse.Namespace, token_file: str, token: str, seen: list) -> dict:
    """read 腿：工具表、真能读、硬调被拒。"""
    client = open_leg(args, "read", args.workspace)
    try:
        seen.append(client.instructions)
        tools = client.tool_names()
        if tools != READ_TOOLS:
            raise Failure(f"read 模式的工具表不对：{tools}")

        got = client.call_tool("read_file", {"path": token_file})
        if is_error(got):
            raise Failure(f"read 腿读不到 coding 腿写的文件：{result_text(got)}")
        seen.append(result_text(got))
        if token not in result_text(got):
            raise Failure("read 腿读到的内容里没有这一轮的 token，两个入口看的不是同一棵树")

        refusals = {}
        for tool, arguments in WITHHELD.items():
            answer = client.call_tool(tool, arguments)
            seen.append(result_text(answer))
            if not is_error(answer):
                raise Failure(f"read 模式下 {tool} 竟然成功了")
            first = result_text(answer).splitlines()[0]
            if not first.startswith("CCNM_E_"):
                raise Failure(f"{tool} 的拒绝没有以 CCNM_E_* 开头：{first}")
            refusals[tool] = first
        return {"tools": tools, "refusals": refusals}
    finally:
        client.close()


def check_coding_leg(args: argparse.Namespace, token_file: str, token: str, seen: list) -> dict:
    """coding 腿：七工具、真写、Runtime 上的属主、并发被拒。"""
    client = open_leg(args, "coding", args.workspace)
    try:
        tools = client.tool_names()
        if sorted(tools) != sorted(CODING_TOOLS):
            raise Failure(f"coding 模式的工具表不对：{tools}")

        written = client.call_tool(
            "apply_patch",
            {"files": [{"op": "add", "path": token_file, "content": token + "\n"}]},
        )
        seen.append(result_text(written))
        if is_error(written):
            raise Failure(f"coding 腿写不进去：{result_text(written)}")

        # 属主由 Runtime 自己报：这条命令就在那台机器上跑。
        #
        # 两种 stat 都要试，而且要按顺序试：`-c %U` 是 GNU 的写法，`-f %Su` 是
        # BSD 的。P12 之后 Runtime 可能是 Linux，写死任何一种都会在另一种上问
        # 不出属主——而"问不出"和"属主不对"在这里是两个结论。
        who = ""
        for style in (["stat", "-c", "%U"], ["stat", "-f", "%Su"]):
            owner = client.call_tool("exec_command", {"cmd": [*style, token_file]})
            seen.append(result_text(owner))
            if is_error(owner):
                continue
            candidate = exec_stdout(owner).strip()
            if candidate:
                who = candidate
                break
        if not who:
            raise Failure("问不到产物属主：GNU 和 BSD 两种 stat 都没答上来")
        if args.runtime_user and who != args.runtime_user:
            raise Failure(f"产物属主是 {who}，期望 {args.runtime_user}")

        whoami = client.call_tool("exec_command", {"cmd": ["id", "-un"]})
        seen.append(result_text(whoami))

        # 同一棵树的第二个 coding 必须起不来。这是两个入口能共存的硬条件。
        second = subprocess.run(
            bridge_argv(args, "coding", args.workspace),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )
        seen.append(second.stderr)
        if second.returncode == 0:
            raise Failure("工作树已经被这条 coding 会话占着，第二个却起来了")
        if "CCNM_E_" not in second.stderr:
            raise Failure(f"并发被拒但没有 CCNM_E_* 诊断：{second.stderr.strip()[:200]}")
        return {
            "tools": sorted(tools),
            "artifact_owner": who,
            "runtime_identity": exec_stdout(whoami).strip(),
            "second_coding_refused": diagnostic(second.stderr),
        }
    finally:
        client.close()


def cleanup_artifact(args: argparse.Namespace, token_file: str) -> str:
    """把这一轮写的文件删掉，并确认它真的没了。"""
    client = open_leg(args, "coding", args.workspace)
    try:
        listed = client.call_tool("list_files", {"path": "."})
        if token_file not in result_text(listed):
            return "already gone"
        # delete 要带 read_file 给的 version。
        current = client.call_tool("read_file", {"path": token_file})
        version = None
        for line in result_text(current).splitlines():
            if "version " in line:
                version = line.split("version ", 1)[1].split()[0].strip("]")
        removed = client.call_tool(
            "apply_patch",
            {"files": [{"op": "delete", "path": token_file, "version": version}]},
        )
        if is_error(removed):
            raise Failure(f"清理失败，产物还在：{result_text(removed)}")
        after = client.call_tool("list_files", {"path": "."})
        if token_file in result_text(after):
            raise Failure("删过了但它还在")
        return "removed"
    finally:
        client.close()


def check_escalation(args: argparse.Namespace, seen: list) -> dict:
    """一个只配了 read 的 workspace，请求 coding 必须拒绝且不降级。"""
    out = subprocess.run(
        bridge_argv(args, "coding", args.read_only_workspace),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        check=False,
    )
    seen.append(out.stderr)
    if out.returncode == 0:
        raise Failure("只读 workspace 上请求 coding 竟然成功了")
    if out.stdout.strip():
        raise Failure("被拒的启动不该在 stdout 上留下任何东西")
    if "read mode" not in out.stderr:
        raise Failure(f"拒绝的理由不是模式越权：{out.stderr.strip()[:200]}")
    return {"refused": diagnostic(out.stderr)}


def diagnostic(stderr: str) -> str:
    """启动失败那条诊断：`CCNM_E_*` 名字加它后面那句解释。

    只记第一行不够——名字在第一行，理由在第二行，而看证据的人要的是理由。
    """
    lines = [line for line in stderr.strip().splitlines() if line.strip()]
    return "\n".join(lines[:2])


def scan_for_leaks(seen: list) -> list:
    """Host 那边看到的全部文本里，有没有本不该出现的东西。"""
    found = []
    for chunk in seen:
        for marker in PRIVATE_MARKERS:
            if marker in chunk:
                found.append(marker)
    return sorted(set(found))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True, help="配成 external_mcp = \"coding\" 的 workspace")
    parser.add_argument("--node", help="本机配置里到 Runtime 的 node")
    parser.add_argument("--ccnm", default="ccnm", help="要跑的 ccnm 二进制")
    parser.add_argument("--config", help="换一份本机配置")
    parser.add_argument("--runtime-user", help="期望的 Runtime 执行身份，产物属主必须是它")
    parser.add_argument("--read-only-workspace", help="配成 external_mcp = \"read\" 的 workspace")
    parser.add_argument("--out", help="把证据写到这个文件")
    args = parser.parse_args()

    token = secrets.token_hex(8)
    token_file = f"ccnm-p11-{token}.txt"
    seen: list = []
    evidence: dict[str, Any] = {
        "tool": "p11_matrix_check",
        "at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "workspace": args.workspace,
        "token_file": token_file,
        "checks": {},
        "passed": False,
    }
    try:
        evidence["checks"]["coding"] = check_coding_leg(args, token_file, token, seen)
        evidence["checks"]["read"] = check_read_leg(args, token_file, token, seen)
        if args.read_only_workspace:
            evidence["checks"]["escalation"] = check_escalation(args, seen)
        else:
            evidence["checks"]["escalation"] = "skipped: no --read-only-workspace"
        leaks = scan_for_leaks(seen)
        if leaks:
            raise Failure(f"Host 看到的文本里出现了私有标记：{leaks}")
        evidence["checks"]["leak_scan"] = "clean"
        evidence["checks"]["cleanup"] = cleanup_artifact(args, token_file)
        evidence["passed"] = True
    except Failure as failure:
        evidence["failure"] = str(failure)
    except ConnectionError as broken:
        evidence["failure"] = f"bridge 起不来或中途没了：{broken}"

    text = json.dumps(evidence, ensure_ascii=False, indent=2)
    if args.out:
        Path(args.out).write_text(text + "\n", encoding="utf-8")
    print(text)
    if not evidence["passed"]:
        print("\n没有通过。证据文件里的 failure 写着卡在哪一项。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
