#!/usr/bin/env python3
"""P12 的真项目 dogfood 工具：一条命令把能机器判的那些判据跑完，写出证据。

**和 P11 那个矩阵工具的区别。** P11 证的是"允许矩阵对不对"：read 给几个工具、
coding 给几个、越权拒不拒。它用的是一棵一次性空树，因为那时候要证的东西和树里
有什么无关。P12 要证的是另一件事：**一个真正的远端项目，用这七个工具真能干
活**——读、搜、改、编译/测、翻输出，改完还能收回去；执行身份手上只有该有的东
西；以及六种出错路径（版本错配、没 opt-in、read 想升 coding、写锁被占、远端消
失、Host 崩）失败之后，不留下孤儿 transport 和不放的写锁。

**怎么判：只看 Runtime 侧的事实。**这里没有模型，也不采信任何自述。

- 改是真改：往目标文件里插一行**必定编译不过**的代码，然后在那台机器上跑构建，
  构建必须失败且错误里指着这个文件——这一条同时证明了三件事：patch 落到了工作
  树、Runtime 上真有工具链、跑的就是我们改过的那棵树；
- 收回去是真收回去：把那一行删掉，再跑一次测试必须过，最后由 Runtime 上的
  `git status --porcelain` 说话——它输出为空才算干净，不是我们说干净；
- 工具链是真可用：`cargo`/`node`/`npm` 的版本由 `exec_command` 在那台机器上问
  出来，不是我们 ssh 进去问的；
- 身份是真受限：uid、组、`sudo -n`、docker socket、`~/.ssh` 里有没有私钥、
  `SSH_AUTH_SOCK`、读不读得到别人的 home，全部由那台机器自己回答。

**它不能替代什么。** 它自己是一个 provider-neutral MCP 客户端，不是 Claude
Code。真实 Host 怎么展示这些工具和它的拒绝，仍然要真的连一次——那半边写在
docs/plan/p12-real-project-session.md 里。

用法（在**客户端**机器上，也就是 bridge 跑的那台）：

    scripts/p12_dogfood_check.py \\
        --workspace p12rust --read-only-workspace p12read --closed-workspace p12off \\
        --node hpsrv --runtime-user ccrun --runtime-home /home/ccrun \\
        --other-home /home/bing --ssh-alias hpsrv-ccrun \\
        --out docs/research/p12-dogfood.json

`--ssh-alias` 那三项（版本错配、远端消失、Host 崩之后远端有没有残留）要一条自
己的 ssh 才能判；不给就跳过并在证据里写明跳过。退出码只有全部通过才是 0——
unknown 不是绿。
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests"))

from mcp_client import McpClient, is_error, result_text  # noqa: E402


# load_skill 是 P36 加的只读工具（项目自带的 skills），read 模式也有。2026-09-11
# 那两轮真机证据是在它之前采的，记的是 4 / 7 个。
READ_TOOLS = ["list_files", "load_skill", "read_file", "search_text", "workspace_info"]
CODING_TOOLS = READ_TOOLS + ["apply_patch", "exec_command", "read_output"]

# read 腿要按名字硬调的三个，参数都合法——参数不合法会先被参数检查拦下，那证
# 明不了权限门禁。
WITHHELD = {
    "exec_command": {"cmd": ["/bin/echo", "hi"]},
    "apply_patch": {"files": [{"op": "add", "path": "ccnm-p12-sneaked.txt", "content": "x\n"}]},
    "read_output": {"output_ref": "r-0000000000000000"},
}

# Host 那边不该看到的东西。
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

# 执行身份不该在的组。staff 在 macOS 上是默认组，在这里出现就说明身份没隔离。
FORBIDDEN_GROUPS = {"sudo", "admin", "wheel", "adm", "docker", "staff", "root"}

# ~/.ssh 里出现这些就是持有出站凭据。authorized_keys 是入站的，允许。
PRIVATE_KEY_HINTS = ("id_", "identity", ".pem", ".key")

# `exec_command` 的第二行就是状态，四种写法：成功是 `ok in N ms`（**不是**
# `exit 0`），失败是 `exit C in N ms`，另外两种是超时和被杀。把"成功"当成"没匹
# 配到退出码"是会静默吃掉判据的，所以这里分开认。
OK_RE = re.compile(r"^ok in \d+ ms", re.M)
EXIT_RE = re.compile(r"^exit (-?\d+) in \d+ ms", re.M)
UNFINISHED_RE = re.compile(r"^(timed out after|killed after) \d+ ms", re.M)
OUTPUT_REF_RE = re.compile(r"output_ref ([^\s\]]+)")
VERSION_RE = re.compile(r"; version ([^\]]+)\]")

# read_output 一页最多 32768；分页要证"偏移稳定"，两页各拿这么多就够了。
PAGE = 512


class Failure(Exception):
    """一项判据没过。消息就是写进证据文件的那句话。"""


# --------------------------------------------------------------- 基础动作


def bridge_argv(args: argparse.Namespace, mode: str, workspace: str) -> list[str]:
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


def start_and_expect_refusal(
    args: argparse.Namespace, mode: str, workspace: str, seen: list
) -> subprocess.CompletedProcess:
    """启动一条 bridge 并要求它失败。stdout 上留下任何东西都算失败。"""
    out = subprocess.run(
        bridge_argv(args, mode, workspace),
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        check=False,
    )
    seen.append(out.stderr)
    if out.returncode == 0:
        raise Failure(f"{workspace} 上以 {mode} 启动竟然成功了")
    if out.stdout.strip():
        raise Failure("被拒的启动不该在 stdout 上留下任何东西")
    if "CCNM_E_" not in out.stderr:
        raise Failure(f"被拒但没有 CCNM_E_* 诊断：{out.stderr.strip()[:200]}")
    return out


def diagnostic(stderr: str) -> str:
    """启动失败那条诊断：`CCNM_E_*` 名字加它后面那句解释。

    只记第一行不够——名字在第一行，理由在第二行，而看证据的人要的是理由。
    """
    lines = [line for line in stderr.strip().splitlines() if line.strip()]
    return "\n".join(lines[:2])


def section(text: str, name: str) -> str:
    """`exec_command` 回的那段文本里 `--- stdout` / `--- stderr` 那一节。"""
    marker = f"--- {name}"
    if marker not in text:
        return ""
    body = text.split(marker, 1)[1]
    for stop in ("--- stdout", "--- stderr", "[output_ref", "[output shortened", "[this runtime"):
        body = body.split(stop, 1)[0]
    return body.strip()


def run_cmd(
    client: McpClient,
    cmd: list[str],
    seen: list,
    *,
    cwd: str | None = None,
    timeout_ms: int | None = None,
) -> dict[str, Any]:
    """跑一条命令。**命令失败不抛异常**——判据是退出码，由调用方决定它该是几。

    工具层被拒（isError）是另一回事，那说明连跑都没跑，单独标出来。
    """
    arguments: dict[str, Any] = {"cmd": cmd}
    if cwd:
        arguments["cwd"] = cwd
    if timeout_ms:
        arguments["timeout_ms"] = timeout_ms
    answer = client.call_tool("exec_command", arguments)
    text = result_text(answer)
    seen.append(text)
    if is_error(answer):
        return {"refused": text.splitlines()[0], "text": text, "exit": None}
    found = EXIT_RE.search(text)
    if OK_RE.search(text):
        exit_code: int | None = 0
    elif found:
        exit_code = int(found.group(1))
    elif UNFINISHED_RE.search(text):
        exit_code = None
    else:
        raise Failure(f"读不懂 exec_command 的状态行：{text[:200]}")
    ref = OUTPUT_REF_RE.search(text)
    return {
        "exit": exit_code,
        "stdout": section(text, "stdout"),
        "stderr": section(text, "stderr"),
        "output_ref": ref.group(1) if ref else None,
        "text": text,
    }


def must_succeed(client: McpClient, cmd: list[str], seen: list, **kw: Any) -> dict[str, Any]:
    got = run_cmd(client, cmd, seen, **kw)
    if got.get("refused"):
        raise Failure(f"{' '.join(cmd)} 连跑都没跑起来：{got['refused']}")
    if got["exit"] != 0:
        raise Failure(f"{' '.join(cmd)} 退出码 {got['exit']}，期望 0：{got['text'][:300]}")
    return got


def file_version(client: McpClient, path: str, seen: list) -> tuple[str, str]:
    """读一个文件，返回 (version, 正文文本)。version 是 patch 的前提。"""
    got = client.call_tool("read_file", {"path": path})
    text = result_text(got)
    seen.append(text)
    if is_error(got):
        raise Failure(f"读不到 {path}：{text}")
    found = VERSION_RE.search(text)
    if not found:
        raise Failure(f"{path} 的 read_file footer 里没有 version：{text[-200:]}")
    return found.group(1), text


def ssh_run(args: argparse.Namespace, remote: str, timeout: int = 60) -> subprocess.CompletedProcess:
    """一条自己的 ssh。只用来问 bridge 问不到的事（远端还有没有进程）。"""
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", args.ssh_alias, remote],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def remote_servers(args: argparse.Namespace) -> list[str]:
    """远端还活着的 `internal mcp-serve` 进程。

    `mcp[-]serve` 这个方括号是为了 pgrep 不要匹配到承载它自己的那条远端 shell
    命令行——那条里也写着 mcp-serve。
    """
    out = ssh_run(args, "pgrep -a -f 'internal mcp[-]serve' || true")
    return [line for line in out.stdout.strip().splitlines() if line.strip()]


# ----------------------------------------------------------------- 各项检查


def check_identity(args: argparse.Namespace, client: McpClient, seen: list) -> dict[str, Any]:
    """P12.2：执行身份手上有什么、没有什么，全部由那台机器自己回答。"""
    who = must_succeed(client, ["id", "-un"], seen)["stdout"].strip()
    if args.runtime_user and who != args.runtime_user:
        raise Failure(f"执行身份是 {who}，期望 {args.runtime_user}")

    groups = must_succeed(client, ["id", "-Gn"], seen)["stdout"].split()
    bad = sorted(FORBIDDEN_GROUPS.intersection(groups))
    if bad:
        raise Failure(f"执行身份在这些组里：{bad}")

    # sudo：命令跑起来但被拒，和机器上根本没有 sudo，都算"不能无密码变 root"。
    # 跑起来还成功了才是失败。
    sudo = run_cmd(client, ["sudo", "-n", "true"], seen)
    if sudo.get("refused"):
        sudo_verdict = f"not runnable: {sudo['refused']}"
    elif sudo["exit"] == 0:
        raise Failure("这个身份有无密码 sudo")
    else:
        sudo_verdict = f"refused with exit {sudo['exit']}"

    docker = run_cmd(client, ["test", "-w", "/var/run/docker.sock"], seen)
    if not docker.get("refused") and docker["exit"] == 0:
        raise Failure("这个身份可以写 /var/run/docker.sock，等于 root")

    # 这一条的输出**故意不进**泄漏扫描那个池子：是我们自己让它打印 ~/.ssh 的
    # 目录名，里面当然有 authorized_keys。扫描要抓的是 ccnm 自己漏出来的私有路
    # 径，不是我们点名要的东西；混在一起会让扫描永远报红，然后被人关掉。
    probed: list = []
    listed = must_succeed(client, ["ls", "-a", f"{args.runtime_home}/.ssh"], probed)["stdout"]
    keys = [
        name
        for name in listed.split()
        if any(hint in name for hint in PRIVATE_KEY_HINTS) and not name.endswith(".pub")
    ]
    if keys:
        raise Failure(f"执行身份的 ~/.ssh 里有出站私钥候选：{keys}")

    # printenv 在变量不存在时退出码非 0，所以"没有"是判得出来的，不是看空字符串。
    agent = run_cmd(client, ["printenv", "SSH_AUTH_SOCK"], seen)
    if not agent.get("refused") and agent["exit"] == 0:
        raise Failure(f"SSH_AUTH_SOCK 在 Runtime 进程里是可用的：{agent['stdout']}")

    other: dict[str, Any] = {}
    if args.other_home:
        readable = run_cmd(client, ["test", "-r", args.other_home], seen)
        if not readable.get("refused") and readable["exit"] == 0:
            raise Failure(f"执行身份读得到 {args.other_home}，身份没隔离")
        other = {"path": args.other_home, "readable": False}

    return {
        "identity": who,
        "groups": groups,
        "sudo": sudo_verdict,
        "docker_socket_writable": False,
        "ssh_dir": sorted(listed.split()),
        "ssh_auth_sock": "unset",
        "other_home": other,
    }


def check_toolchain(
    args: argparse.Namespace, client: McpClient, seen: list
) -> dict[str, str]:
    """P12.2 的后半句：项目 toolchain 实际可用。

    这是一条最容易被文档糊过去的判据。`exec_command` 的命令跑在一条非交互 ssh
    会话里，装在执行身份 home 里的工具链如果没写进那条会话看得见的 PATH，ccnm
    回的是"cargo is not installed on the Runtime Node, or is not on its PATH"。
    所以这里问的不是"装了没有"，而是"从这条路走过去，叫得动吗"。
    """
    versions: dict[str, str] = {}
    for line in args.toolchain:
        cmd = shlex.split(line)
        got = run_cmd(client, cmd, seen)
        if got.get("refused"):
            raise Failure(f"{line} 在 Runtime 上叫不动：{got['refused']}")
        if got["exit"] != 0:
            raise Failure(f"{line} 退出码 {got['exit']}：{got['text'][:200]}")
        versions[cmd[0]] = (got["stdout"] or got["stderr"]).splitlines()[0].strip()
    return versions


def check_cycle(args: argparse.Namespace, client: McpClient, seen: list) -> dict[str, Any]:
    """P12.1：read → search → patch → exec/test → read_output，再把改动收回去。"""
    info = client.call_tool("workspace_info", {})
    seen.append(result_text(info))
    if is_error(info):
        raise Failure(f"workspace_info 失败：{result_text(info)}")

    listed = client.call_tool("list_files", {"path": "."})
    seen.append(result_text(listed))
    if args.project_marker not in result_text(listed):
        raise Failure(f"根目录里没有 {args.project_marker}，这不像那个项目")

    found = client.call_tool("search_text", {"query": args.anchor, "path": "."})
    hits = result_text(found)
    seen.append(hits)
    if is_error(found) or args.patch_target not in hits:
        raise Failure(f"搜不到 {args.anchor} 在 {args.patch_target} 里：{hits[:300]}")

    version, _ = file_version(client, args.patch_target, seen)

    # 插一行必定编译不过的代码。判据不是"patch 返回成功"，是下面那次构建失败。
    poisoned = client.call_tool(
        "apply_patch",
        {
            "files": [
                {
                    "op": "update",
                    "path": args.patch_target,
                    "version": version,
                    "edits": [{"old": args.anchor, "new": f"{args.anchor}\n{args.poison}"}],
                }
            ]
        },
    )
    seen.append(result_text(poisoned))
    if is_error(poisoned):
        raise Failure(f"改不进去：{result_text(poisoned)}")

    try:
        build = run_cmd(
            client, shlex.split(args.build_cmd), seen, timeout_ms=args.build_timeout_ms
        )
        if build.get("refused"):
            raise Failure(f"构建命令连跑都没跑：{build['refused']}")
        if build["exit"] == 0:
            raise Failure("插了一行编译不过的代码，构建却成功了——那棵树不是我们改的那棵")
        if args.patch_target not in build["text"] and args.patch_target not in build["stderr"]:
            raise Failure(f"构建失败了但错误里没指着 {args.patch_target}：{build['text'][:400]}")
        if not build["output_ref"]:
            raise Failure("失败的构建没有给 output_ref，翻不了完整输出")

        # read_output：偏移稳定、拿得到预览之外的东西。编译错误在 stderr。
        first = client.call_tool(
            "read_output",
            {"output_ref": build["output_ref"], "stream": "stderr", "offset": 0, "limit": PAGE},
        )
        second = client.call_tool(
            "read_output",
            {"output_ref": build["output_ref"], "stream": "stderr", "offset": PAGE, "limit": PAGE},
        )
        for answer in (first, second):
            seen.append(result_text(answer))
            if is_error(answer):
                raise Failure(f"read_output 失败：{result_text(answer)}")
        if result_text(first) == result_text(second):
            raise Failure("read_output 两页一模一样，偏移没起作用")
        if not (result_text(first) + result_text(second)).strip():
            raise Failure("read_output 什么都没给")
    finally:
        # 不管上面哪一步炸了，都要把那一行收回去，别把一棵改坏的树留在远端。
        version, _ = file_version(client, args.patch_target, seen)
        restored = client.call_tool(
            "apply_patch",
            {
                "files": [
                    {
                        "op": "update",
                        "path": args.patch_target,
                        "version": version,
                        "edits": [{"old": f"{args.anchor}\n{args.poison}", "new": args.anchor}],
                    }
                ]
            },
        )
        seen.append(result_text(restored))
        if is_error(restored):
            raise Failure(f"收不回去，远端留着一棵改坏的树：{result_text(restored)}")

    # 干净不干净由那台机器上的 git 说，不是我们说。
    status = must_succeed(client, ["git", "status", "--porcelain"], seen)
    if status["stdout"].strip():
        raise Failure(f"收回去之后工作树还是脏的：{status['stdout'][:300]}")

    test = must_succeed(
        client, shlex.split(args.test_cmd), seen, timeout_ms=args.test_timeout_ms
    )
    cycle = {
        "project_marker": args.project_marker,
        "search_hit": args.patch_target,
        "poisoned_build_exit": build["exit"],
        "build_error_named_the_file": True,
        "read_output_pages": [len(result_text(first)), len(result_text(second))],
        "tree_clean_after_revert": True,
        "test_exit": test["exit"],
        "test_tail": test["stdout"].splitlines()[-1:] or test["stderr"].splitlines()[-1:],
    }
    if args.full_test_cmd:
        full = must_succeed(
            client, shlex.split(args.full_test_cmd), seen, timeout_ms=args.test_timeout_ms
        )
        cycle["full_test_exit"] = full["exit"]
        # 预览只有头尾，中间那些 `test result` 行看不见；所以这里记看得见的那
        # 几行，真正的判据是退出码——任何一个测试红了 cargo 就非 0。
        summaries = [
            line
            for line in f"{full['stdout']}\n{full['stderr']}".splitlines()
            if line.startswith("test result")
        ]
        cycle["full_test_visible_summaries"] = summaries[-3:]
    return cycle


def check_read_leg(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """read 腿：正好四个工具、真能读同一棵树、三个没给的工具硬调被拒。

    这里开的是**同一个 workspace 的 read 模式**，不是另一个只读 workspace：客
    户端可以要得比配置少。两个 workspace 也不可能指同一棵树——Runtime 会以
    "roots overlap after canonicalization"拒绝写授权。所以"两个入口看同一棵树"
    只能这么证。
    """
    client = open_leg(args, "read", args.workspace)
    try:
        seen.append(client.instructions)
        tools = client.tool_names()
        if tools != READ_TOOLS:
            raise Failure(f"read 模式的工具表不对：{tools}")
        got = client.call_tool("read_file", {"path": args.patch_target})
        seen.append(result_text(got))
        if is_error(got):
            raise Failure(f"read 腿读不到 {args.patch_target}：{result_text(got)}")
        if args.anchor not in result_text(got):
            raise Failure("read 腿读到的内容不对，两个入口看的不是同一棵树")
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


def check_version_mismatch(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """P12.3：对端不认识的 open protocol 必须停下，不能降级。

    这一条不经过 bridge：自己拼一个协议号错的 payload，直接交给远端那个真的
    ccnm。判的是真二进制的分派，只是把"对端是个旧版本"换成"对端不认识这个号"
    ——同一段代码，同一个决定。
    """
    payload = json.dumps(
        {"protocol": 99, "workspace": args.workspace, "session": "p12-version-probe", "mode": "read"}
    ).encode("utf-8")
    encoded = base64.urlsafe_b64encode(payload).decode("ascii").rstrip("=")
    out = ssh_run(args, f"{args.remote_ccnm} internal mcp-serve --payload {encoded}")
    seen.append(out.stderr)
    if out.returncode == 0:
        raise Failure("协议号错了，远端却接受了")
    if "protocol" not in out.stderr:
        raise Failure(f"拒绝的理由不是协议号：{out.stderr.strip()[:200]}")
    if out.stdout.strip():
        raise Failure("协议不匹配还在 stdout 上说了话，那就不是停下")
    return {"exit": out.returncode, "refused": diagnostic(out.stderr)}


def guard_is_stale(stderr: str) -> bool:
    """被拒的理由是"上一个 writer 被打断，锁还留在 held"。

    这条判据写得这么死是因为它是**设计**，不是缺陷：进程被 kill 之后 flock 随
    进程释放，但锁文件里留着 `held`，所以下一个 coding 会话被拒而不是自动接管
    ——旧的子进程可能还活着。所以这里要求的不是"锁自己好了"，而是"它明确地没
    自己好，并且说清了为什么"。
    """
    return "left held by an interrupted process" in stderr


def clear_stale_guard(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """按支持矩阵写的人工恢复边界，把留在 `held` 的那把锁清掉。

    这不是绕过门禁：调用它之前已经证明远端没有残留进程了，而 ccnm 有意不自动
    接管——那一步要人来做。脚本代人做这一步，并把删掉的文件名记进证据；它不循
    环删锁，也不按时间强制接管。
    """
    if args.guard_dir:
        directory = args.guard_dir
    elif args.ssh_alias:
        directory = args.remote_guard_dir
    else:
        return {"cleared": "skipped: neither --guard-dir nor --ssh-alias"}
    # 只删内容以 `held ` 开头的那些；released 的和别的文件一律不动。
    script = (
        f'for f in {directory}/*.lock; do [ -f "$f" ] || continue; '
        'if head -c 5 "$f" | grep -q "^held"; then echo "$f"; rm -f "$f"; fi; done'
    )
    if args.guard_dir:
        out = subprocess.run(
            ["/bin/sh", "-c", script], capture_output=True, text=True, check=False
        )
    else:
        out = ssh_run(args, script)
    removed = [line for line in out.stdout.strip().splitlines() if line.strip()]
    if not removed:
        raise Failure(f"没有找到留在 held 的锁文件，恢复动作没发生：{out.stderr.strip()[:200]}")
    return {"cleared": removed}


def guard_state(args: argparse.Namespace) -> str:
    """那把锁现在写着什么：`held <session> <workspace>`、`released` 或没有文件。"""
    script = f'cat {args.guard_dir or args.remote_guard_dir}/*.lock 2>/dev/null || true'
    if args.guard_dir:
        out = subprocess.run(
            ["/bin/sh", "-c", script], capture_output=True, text=True, check=False
        )
    elif args.ssh_alias:
        out = ssh_run(args, script)
    else:
        return "unknown: neither --guard-dir nor --ssh-alias"
    return out.stdout.strip() or "no lock file"


def after_a_killed_server(args: argparse.Namespace, seen: list, what: str) -> dict[str, Any]:
    """远端**服务端进程**被杀之后，该是什么样子。

    三件事一起看才有意义：coding 被明确拒绝（不自动接管）、read 照常开（读不
    受写锁影响）、人工恢复之后 coding 又能开（这条路不是死路）。

    注意这只适用于"服务端自己被杀"：它没机会跑收尾代码，所以锁停在 held。Host
    被杀是另一回事，见 check_host_crash。
    """
    refused = start_and_expect_refusal(args, "coding", args.workspace, seen)
    if not guard_is_stale(refused.stderr):
        raise Failure(f"{what} 之后 coding 被拒，但理由不是锁留在 held：{refused.stderr.strip()[:300]}")
    # read 不拿写锁，所以它必须照常开——这条是"锁只挡写者"的证据。
    reader = open_leg(args, "read", args.workspace)
    try:
        read_ok = reader.tool_names() == READ_TOOLS
    finally:
        reader.close()
    if not read_ok:
        raise Failure(f"{what} 之后 read 腿也开不了")
    recovered = clear_stale_guard(args, seen)
    if str(recovered.get("cleared", "")).startswith("skipped"):
        # 没法做那一步就不假装做过：锁还留着，这一轮结束后要人去清。
        return {
            "coding_refused": diagnostic(refused.stderr),
            "read_still_opens": read_ok,
            "manual_recovery": recovered,
            "coding_after_recovery": "not attempted; the guard is still held",
        }
    again = open_leg(args, "coding", args.workspace)
    try:
        tools = again.tool_names()
        if sorted(tools) != sorted(CODING_TOOLS):
            raise Failure(f"{what} 恢复之后工具表不对：{tools}")
    finally:
        again.close()
    return {
        "coding_refused": diagnostic(refused.stderr),
        "read_still_opens": read_ok,
        "manual_recovery": recovered,
        "coding_after_recovery": "ok",
    }


def check_remote_gone(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """P12.3：远端进程消失时，客户端读到干净的结束，远端不留孤儿。"""
    client = open_leg(args, "coding", args.workspace)
    before = remote_servers(args)
    if not before:
        raise Failure("会话开着，远端却看不到 mcp-serve 进程")
    pids = [line.split()[0] for line in before]
    killed = ssh_run(args, f"kill -9 {' '.join(pids)}")
    seen.append(killed.stderr)
    verdict = "unknown"
    try:
        answer = client.call_tool("workspace_info", {})
        seen.append(result_text(answer))
        raise Failure("远端被 kill 了，工具调用却还成功")
    except ConnectionError as broken:
        verdict = str(broken).splitlines()[0]
    finally:
        client.close()
    left = remote_servers(args)
    if left:
        raise Failure(f"远端还留着 mcp-serve：{left}")
    return {
        "killed_pids": pids,
        "client_saw": verdict,
        **after_a_killed_server(args, seen, "远端消失"),
    }


def check_host_crash(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """P12.3：Host 被 kill -9 之后，远端不留孤儿，而且写锁**是放了的**。

    这和"远端被杀"结局不同，区别在于谁还有机会跑收尾代码：

    - Host（也就是 bridge，它 exec 成了 ssh）被杀 → 远端 mcp-serve 读到 EOF，
      自己正常退出，Drop 把锁标成 released → 下一个 coding 直接能开，不需要人；
    - 远端服务端被杀 → 没有 Drop → 锁停在 held → 必须人来清。
        # 真机上这条先写错过一次：把两种结局当成一种，于是"Host 崩之后锁该留在
        # held"这个判据要求了一件**不该发生**的事。磁盘上的锁说了实话。
    """
    client = open_leg(args, "coding", args.workspace)
    pid = client.pid
    client.kill()
    left: list[str] = []
    if args.ssh_alias:
        # sshd 要先发现连接断了才会收掉远端那条命令，给它几秒。
        for _ in range(20):
            left = remote_servers(args)
            if not left:
                break
            time.sleep(1)
        if left:
            raise Failure(f"Host 被 kill 之后远端还留着：{left}")
    lock = guard_state(args)
    outcome: dict[str, Any] = {"guard_after_crash": lock}
    if lock.startswith("held "):
        # 远端没来得及跑收尾——在这个工具的离线自测里必然如此，因为那里"Host"和
        # 服务端是同一个进程，kill -9 连收尾代码一起杀了。判据于是变成"锁的状态
        # 是确定的，而从这个状态往下走的那条路是通的"，两种结局都不含糊。
        outcome.update(after_a_killed_server(args, seen, "Host 崩"))
    else:
        again = open_leg(args, "coding", args.workspace)
        try:
            tools = again.tool_names()
            if sorted(tools) != sorted(CODING_TOOLS):
                raise Failure(f"Host 崩过之后工具表不对：{tools}")
        finally:
            again.close()
        outcome["coding_without_recovery"] = "ok"
    return {
        "killed_bridge_pid": pid,
        "remote_orphans": left,
        "orphan_check": "done" if args.ssh_alias else "skipped: no --ssh-alias",
        **outcome,
    }


def check_reconnect(args: argparse.Namespace, seen: list) -> dict[str, Any]:
    """P12.1 的断开/重连：正常关掉再开，看到的还是同一棵树。"""
    first = open_leg(args, "coding", args.workspace)
    try:
        version, _ = file_version(first, args.patch_target, seen)
    finally:
        code = first.close()
    second = open_leg(args, "coding", args.workspace)
    try:
        again, _ = file_version(second, args.patch_target, seen)
    finally:
        second.close()
    if version != again:
        raise Failure(f"重连之后 {args.patch_target} 的 version 变了：{version} → {again}")
    return {"first_exit": code, "same_version": version}


def scan_for_leaks(seen: list) -> list[str]:
    found = []
    for chunk in seen:
        for marker in PRIVATE_MARKERS:
            if marker in chunk:
                found.append(marker)
    return sorted(set(found))


# --------------------------------------------------------------------- main


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", required=True, help='配成 external_mcp = "coding" 的真项目')
    parser.add_argument("--read-only-workspace", help='同一棵树上配成 external_mcp = "read" 的那个')
    parser.add_argument("--closed-workspace", help="根本没有 opt-in 的 workspace")
    parser.add_argument("--node", help="本机配置里到 Runtime 的 node")
    parser.add_argument("--ccnm", default="ccnm", help="要跑的 ccnm 二进制")
    parser.add_argument("--config", help="换一份本机配置")
    parser.add_argument("--runtime-user", help="期望的 Runtime 执行身份")
    parser.add_argument(
        "--skip-identity-audit",
        action="store_true",
        help="跳过身份审计。**真机轮不要用**：开发者自己的账号本来就在 admin/staff 里，"
        "这个开关只给这个工具自己的离线自测用，证据里会写明跳过，跳过不是通过",
    )
    parser.add_argument("--runtime-home", default="/home/ccrun", help="执行身份的 home")
    parser.add_argument("--other-home", help="一个必须读不到的别人的 home")
    parser.add_argument("--ssh-alias", help="到 Runtime 执行身份的 ssh alias，用于问远端残留")
    parser.add_argument("--remote-ccnm", default="~/.local/bin/ccnm", help="远端 ccnm 的路径")
    parser.add_argument(
        "--remote-guard-dir",
        default="$HOME/.local/state/ccnm/write-guards",
        help="远端写锁目录，人工恢复那一步要用",
    )
    parser.add_argument(
        "--guard-dir", help="写锁目录在本机时用这个（离线自测走这条），优先于 --remote-guard-dir"
    )
    parser.add_argument("--project-marker", default="Cargo.toml", help="根目录里必须有的文件")
    parser.add_argument(
        "--patch-target", default="crates/ccnm-core/src/lib.rs", help="要改的真实源文件"
    )
    parser.add_argument(
        "--anchor",
        default="pub use config::Config;",
        help="目标文件里唯一出现一次的锚点，改动插在它后面",
    )
    parser.add_argument(
        "--poison",
        default='const P12_DOGFOOD_POISON: u32 = "this must not compile";',
        help="一行必定编译不过的代码",
    )
    parser.add_argument("--build-cmd", default="cargo build -p ccnm-core", help="带毒时必须失败")
    parser.add_argument(
        "--test-cmd", default="cargo test -p ccnm-core --lib runtime::", help="干净时必须通过"
    )
    parser.add_argument("--full-test-cmd", help="可选：整套测试，通过才算数")
    parser.add_argument("--build-timeout-ms", type=int, default=600_000)
    parser.add_argument("--test-timeout-ms", type=int, default=600_000)
    parser.add_argument(
        "--toolchain",
        action="append",
        default=None,
        help="要在 Runtime 上问版本的命令，可给多次",
    )
    parser.add_argument("--out", help="把证据写到这个文件")
    args = parser.parse_args()
    if args.toolchain is None:
        args.toolchain = [
            "cargo --version",
            "rustc --version",
            "node --version",
            "npm --version",
            "git --version",
        ]

    seen: list = []
    evidence: dict[str, Any] = {
        "tool": "p12_dogfood_check",
        "at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "workspace": args.workspace,
        "node": args.node,
        "expected_runtime_user": args.runtime_user,
        "checks": {},
        "passed": False,
    }
    checks = evidence["checks"]
    try:
        client = open_leg(args, "coding", args.workspace)
        try:
            tools = client.tool_names()
            if sorted(tools) != sorted(CODING_TOOLS):
                raise Failure(f"coding 模式的工具表不对：{tools}")
            checks["tools"] = sorted(tools)
            checks["identity"] = (
                "skipped: --skip-identity-audit (not a pass)"
                if args.skip_identity_audit
                else check_identity(args, client, seen)
            )
            checks["toolchain"] = check_toolchain(args, client, seen)
            checks["cycle"] = check_cycle(args, client, seen)
            # 写锁被占：这条会话还开着，第二个 coding 必须起不来。
            busy = start_and_expect_refusal(args, "coding", args.workspace, seen)
            checks["writer_busy"] = diagnostic(busy.stderr)
        finally:
            client.close()

        if args.read_only_workspace:
            checks["read_leg"] = check_read_leg(args, seen)
            escalated = start_and_expect_refusal(args, "coding", args.read_only_workspace, seen)
            if "read mode" not in escalated.stderr:
                raise Failure(f"拒绝的理由不是模式越权：{escalated.stderr.strip()[:200]}")
            checks["escalation"] = diagnostic(escalated.stderr)
        else:
            checks["read_leg"] = "skipped: no --read-only-workspace"
            checks["escalation"] = "skipped: no --read-only-workspace"

        if args.closed_workspace:
            closed = start_and_expect_refusal(args, "read", args.closed_workspace, seen)
            checks["not_opted_in"] = diagnostic(closed.stderr)
        else:
            checks["not_opted_in"] = "skipped: no --closed-workspace"

        checks["reconnect"] = check_reconnect(args, seen)

        if args.ssh_alias:
            checks["version_mismatch"] = check_version_mismatch(args, seen)
            checks["remote_gone"] = check_remote_gone(args, seen)
        else:
            checks["version_mismatch"] = "skipped: no --ssh-alias"
            checks["remote_gone"] = "skipped: no --ssh-alias"

        checks["host_crash"] = check_host_crash(args, seen)

        leaks = scan_for_leaks(seen)
        if leaks:
            raise Failure(f"Host 看到的文本里出现了私有标记：{leaks}")
        checks["leak_scan"] = "clean"
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
