#!/usr/bin/env python3
"""P7.3 的对照工具：同一件事分别走人类 CLI 和 machine API，比对结果。

**为什么需要它。** P7.3 要求"用公共 API 各跑一次真实 provider 的双机闭环，
与人类 CLI 结果对照"。手工做这件事要开两个终端、记两份输出、再肉眼比对，
而这一步发生在消耗真实订阅额度的真机会话里——来回一轮就是一次额度。这个
脚本把一次对照压缩成一条命令，并把结论写成机器可读的证据文件。

**怎么比。** 不比模型输出的文字。同一个提示词问两次，措辞本来就不会一样，
拿它当判据只会得到一个永远红或者永远假绿的检查。改为比**副作用**：让两条
腿各自在 Runtime 的工作目录里写一个带一次性 token 的文件，然后由这个脚本
（跑在 Runtime Node 上，工作树就在本地）去看文件在不在、内容对不对、属主是
谁。这是 P3 真机验收用过的取证方式——Claude 经 MCP 编译出 Mach-O 并写
result.txt，属主是 Runtime 身份——模型说自己做了不算数，磁盘上的东西才算。

**属主为什么重要。** 两条腿的产物属主必须是同一个 Runtime 执行身份。这一条
证明两个公开入口最终落到同一条隔离执行链上，而不是 machine API 偷偷用了别
的身份。

用法（在 Runtime Node 上，配置里已有这个 workspace 和绑定）：

    scripts/p7_parity_check.py \\
        --workspace demo --instance claude-main --provider claude \\
        --out docs/research/p7-parity-claude.json

不给 `--root` 就从 `ccnm rpc` 之外的地方猜工作树，脚本不猜——直接报错要你
给。exit code 只有全部检查通过才是 0；任何一项判不出来都算没通过。
"""

from __future__ import annotations

import argparse
import json
import os
import pwd
import secrets
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "clients/python"))

from ccnm_machine_client import MachineClient, RpcError  # noqa: E402


# machine API 的响应里不该出现的东西。tests/test_blackbox_client.py 用合成数据
# 离线证明过同一条规则；这里要再查一遍，因为只有真机上才存在真的凭据路径——
# 离线测试里那些字符串本来就不可能出现，它证明不了真环境下也不出现。
PRIVATE_MARKERS = [
    ".claude",
    ".codex",
    "auth.json",
    "credentials",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "id_rsa",
    "id_ed25519",
    "session_dir",
    "controller",
]


def probe_prompt(path: Path, token: str) -> str:
    """让 Agent 留下一个可验证的副作用。

    提示词写得死板是故意的：文件名和内容都要求逐字复现，模型少写一个字符就
    是没通过，不给"大概做到了"留解释空间。用英文是因为要求的是精确字符串复
    现，少一层翻译少一层歧义。
    """
    return (
        f"Create a file at exactly this path: {path}\n"
        f"Its entire content must be exactly this line, nothing else: {token}\n"
        "Do not create any other file. Do not modify any existing file. "
        "Reply with only the word DONE when the file is written."
    )


def owner_of(path: Path) -> dict[str, Any]:
    """产物的属主。名字查不到就只记 uid，不编一个名字出来。"""
    uid = path.stat().st_uid
    try:
        name = pwd.getpwuid(uid).pw_name
    except KeyError:
        name = None
    return {"uid": uid, "name": name}


def check_side_effect(path: Path, token: str) -> dict[str, Any]:
    """看 Agent 到底有没有在 Runtime 上写下那个文件。"""
    if not path.exists():
        return {"present": False, "matches": False, "reason": "文件不存在"}
    if path.is_symlink() or not path.is_file():
        return {"present": True, "matches": False, "reason": "不是普通文件"}
    try:
        content = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        return {"present": True, "matches": False, "reason": f"读不出来：{exc}"}
    matches = content.strip() == token
    return {
        "present": True,
        "matches": matches,
        "owner": owner_of(path),
        "bytes": path.stat().st_size,
        "reason": None if matches else "内容与 token 不一致",
    }


def guards_held(guard_dir: Path | None) -> list[str] | None:
    """哪些工作树的写入 guard 还锁着。读不到就返回 None，不假装没有。

    `write-guards/` 里每个文件要么是 `released`，要么是 `held <session>
    <workspace>`。[运维手册](../docs/operations.md)本来就让人工去这个目录看，
    所以这不算碰内部状态。

    **guard 由 Runtime 执行身份写**，它的 XDG_STATE_HOME 通常不是操作者的，
    多半读不到——所以这个目录要显式给。
    """
    if guard_dir is None or not guard_dir.is_dir():
        return None
    held = []
    for path in sorted(guard_dir.iterdir()):
        try:
            text = path.read_text(encoding="utf-8")
        except OSError:
            return None
        if text.startswith("held "):
            held.append(text.strip())
    return held


def settle_between_legs(guard_dir: Path | None, seconds: float) -> dict[str, Any]:
    """两条腿之间等写入 guard 放开。

    **为什么必须等。** guard 由 Runtime 侧的 MCP 进程持有，进程退出时才释放；
    而 `ccnm run --print` 是走另一条 SSH 通道同步返回的，两者之间没有任何同
    步。第一条腿返回的那一刻，Runtime 那边的 MCP 可能还没退干净，紧接着起第
    二条腿就会撞上 "workspace write guard is busy"——在花额度的会话里，那会
    显示成"machine API 失败"，实际只是排队没排开。

    能读到 guard 目录就等到真放开（有依据）；读不到就固定等一段（是假设）。
    两者的区别写进证据，读的人得知道这一步是验过的还是猜的。
    """
    deadline = time.monotonic() + seconds
    if guards_held(guard_dir) is None:
        time.sleep(seconds)
        return {"method": "fixed-delay", "waited_s": seconds, "observed": False}

    while True:
        held = guards_held(guard_dir)
        waited = round(time.monotonic() - (deadline - seconds), 1)
        if held == []:
            return {"method": "guard-dir", "waited_s": waited, "observed": True}
        if time.monotonic() >= deadline:
            return {
                "method": "guard-dir",
                "waited_s": waited,
                "observed": True,
                "still_held": held,
            }
        time.sleep(0.5)


def clear_target(path: Path) -> None:
    """开跑前确认产物路径是空的。

    留着上一轮的同名文件会让这一轮不劳而获地"通过"。token 每轮新生成，正常
    情况下不会撞上；真撞上了说明有人手工造过，那必须停下来问清楚，不能替他
    删。
    """
    if path.exists() or path.is_symlink():
        raise SystemExit(f"产物路径已经有东西了，先确认它是什么再重跑：{path}")


def remove_artifact(path: Path, token: str) -> bool:
    """清掉本轮自己造出来的那个文件。

    只删名字里带本轮 token、内容也正好是本轮 token 的那一个。内容对不上就留
    着——那是"没通过"的现场，删了就没法看了。
    """
    if not path.is_file() or path.is_symlink():
        return False
    if token not in path.name:
        return False
    if path.read_text(encoding="utf-8").strip() != token:
        return False
    path.unlink()
    return True


def run_human_cli(
    ccnm: str,
    config: str | None,
    workspace: str,
    instance: str | None,
    prompt: str,
    timeout: int,
) -> dict[str, Any]:
    """第一条腿：人类走的那个入口。"""
    argv = [ccnm]
    if config:
        argv += ["--config", config]
    argv += ["run", workspace]
    if instance:
        argv += ["--agent", instance]
    argv += ["--print", prompt, "--timeout", str(timeout)]

    started = time.monotonic()
    try:
        done = subprocess.run(
            argv,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            # 比 --timeout 多给一点，好让 ccnm 自己的超时先生效——被外面掐掉
            # 和它自己判超时是两种结果，混在一起就分不清是谁的问题。
            timeout=timeout + 120,
        )
    except subprocess.TimeoutExpired:
        return {
            "ok": False,
            "reason": "ccnm run 超过外层等待时间仍未返回",
            "elapsed_s": round(time.monotonic() - started, 1),
        }
    return {
        "ok": done.returncode == 0,
        "exit_code": done.returncode,
        "elapsed_s": round(time.monotonic() - started, 1),
        # 输出可能很长，证据里只留尾巴；完整内容在操作者的终端里。
        "stdout_tail": done.stdout[-2000:],
        "stderr_tail": done.stderr[-2000:],
        "reason": None if done.returncode == 0 else f"退出码 {done.returncode}",
    }


def run_machine_api(
    ccnm: str,
    config: str | None,
    workspace: str,
    node: str | None,
    instance: str | None,
    prompt: str,
    token: str,
    timeout: int,
) -> dict[str, Any]:
    """第二条腿：程序走的那个入口。

    `start_key` 给的是本轮 token。协议说明建议"凡结果有意义的执行都给一个
    键"，这里正好自己吃一次：真机会话中途断了，凭键就能找回同一个 session，
    不会重复消耗一次额度。
    """
    agent = None
    if node or instance:
        agent = {}
        if node:
            agent["node"] = node
        if instance:
            agent["instance"] = instance

    leg: dict[str, Any] = {"ok": False, "reason": None, "responses": []}
    started = time.monotonic()
    try:
        with MachineClient(ccnm=ccnm, config=config) as client:
            hello = client.hello("ccnm-p7-parity/1")
            leg["responses"].append(hello)
            leg["server"] = hello.get("server")
            leg["protocol"] = hello.get("protocol")

            listed = client.agents_list()
            leg["responses"].append({"agents": listed})

            begun = client.session_start(
                workspace=workspace,
                prompt=prompt,
                agent=agent,
                start_key=token,
                timeout_ms=timeout * 1000,
            )
            leg["responses"].append(begun)
            session = begun["session"]
            leg["session"] = session
            leg["reused"] = begun.get("reused")

            result = client.wait(session, timeout=timeout + 120)
            leg["responses"].append(result)
            leg["state"] = result.get("state")
            leg["outcome"] = result.get("outcome")
            leg["provider"] = (result.get("agent") or {}).get("provider")
            leg["provider_session_id_present"] = "provider_session_id" in result
            # 只记不判。两条腿的提示词不一样（文件名带各自的 leg），token 数
            # 本来就不该相同，拿它做判据没意义。但这两个字段是刚接通的，真机
            # 那次的记录里有没有数字，是它端到端通没通的唯一证据。
            leg["usage"] = result.get("usage")
            leg["cost"] = result.get("cost")
            leg["text_tail"] = (result.get("text") or "")[-2000:]
    except TimeoutError as exc:
        leg["reason"] = f"轮询超时：{exc}"
    except RpcError as exc:
        leg["reason"] = f"协议错误 {exc.code}：{exc.message}（effect={exc.effect}）"
        leg["error"] = {"code": exc.code, "message": exc.message, "data": exc.data}
    except (OSError, ValueError, KeyError) as exc:
        leg["reason"] = f"连接或响应异常：{type(exc).__name__}: {exc}"

    leg["elapsed_s"] = round(time.monotonic() - started, 1)
    if leg["reason"] is None:
        # completed 只说明 Agent 进程正常结束。副作用那一项另外查。
        if leg.get("state") != "completed":
            leg["reason"] = f"终态是 {leg.get('state')}，不是 completed"
        elif (leg.get("outcome") or {}).get("exit_code") != 0:
            leg["reason"] = f"outcome.exit_code = {(leg.get('outcome') or {}).get('exit_code')}"
        else:
            leg["ok"] = True
    return leg


def scan_for_private(responses: list[Any], home: str) -> list[str]:
    """把 machine API 的全部响应拼成一个串，逐条找不该出现的东西。"""
    blob = json.dumps(responses, ensure_ascii=False)
    return [marker for marker in [home, *PRIVATE_MARKERS] if marker and marker in blob]


def blames_the_guard(leg: dict[str, Any]) -> bool:
    """这条腿是不是栽在写入 guard 上。

    只是个诊断，不参与判定：guard 没排开和协议出错都会让会话失败，但前者重跑
    有意义、后者重跑只是再花一次额度。分不清这两者，读记录的人就会往错的方向
    查。匹配的是 crates/ccnm-core/src/mcp/write_guard.rs 给操作者看的那两句话。
    """
    if leg.get("ok"):
        return False
    blob = json.dumps(leg, ensure_ascii=False)
    return "write guard" in blob


def verdict(checks: list[dict[str, Any]]) -> str:
    """判不出来一律不算通过。

    真机会话里最坏的失败模式不是红，是一个什么都没跑成却报绿的工具——那会
    让一次付费验收得出错误结论，而且没人会去复查绿灯。
    """
    if any(check["passed"] is False for check in checks):
        return "fail"
    if any(check["passed"] is None for check in checks):
        return "inconclusive"
    return "pass"


def build_checks(
    human: dict[str, Any],
    human_effect: dict[str, Any],
    machine: dict[str, Any],
    machine_effect: dict[str, Any],
    leaked: list[str],
    expect_provider: str | None,
) -> list[dict[str, Any]]:
    def check(name: str, passed: bool | None, detail: str) -> dict[str, Any]:
        return {"name": name, "passed": passed, "detail": detail}

    human_owner = (human_effect.get("owner") or {}).get("uid")
    machine_owner = (machine_effect.get("owner") or {}).get("uid")
    if human_owner is None or machine_owner is None:
        same_owner: bool | None = None
        owner_detail = "有一条腿没产物，属主无从比对"
    else:
        same_owner = human_owner == machine_owner
        owner_detail = f"人类 CLI uid={human_owner}，machine API uid={machine_owner}"

    if expect_provider is None:
        provider_ok: bool | None = None
        provider_detail = "没有给 --provider，不做比对"
    elif machine.get("provider") is None:
        provider_ok = None
        provider_detail = "响应里没有 agent.provider"
    else:
        provider_ok = machine["provider"] == expect_provider
        provider_detail = f"声明 {expect_provider}，Agent 报 {machine['provider']}"

    return [
        check("human_cli_succeeded", human["ok"], human.get("reason") or "退出码 0"),
        check(
            "human_cli_side_effect",
            human_effect["matches"],
            human_effect.get("reason") or "工作树里有内容正确的产物",
        ),
        check("machine_api_succeeded", machine["ok"], machine.get("reason") or "completed 且 exit_code 0"),
        check(
            "machine_api_side_effect",
            machine_effect["matches"],
            machine_effect.get("reason") or "工作树里有内容正确的产物",
        ),
        check("same_runtime_owner", same_owner, owner_detail),
        check(
            "machine_api_kept_private_data_out",
            not leaked,
            "响应里没有私有路径或凭据名" if not leaked else f"漏出：{leaked}",
        ),
        check("provider_matches_declaration", provider_ok, provider_detail),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--workspace", required=True, help="配置里的 workspace 名")
    parser.add_argument("--root", required=True, type=Path, help="该 workspace 在本机的工作树路径")
    parser.add_argument("--instance", help="覆盖 workspace 默认的 Agent Instance")
    parser.add_argument("--node", help="Agent Node 名，只在需要显式指定时给")
    parser.add_argument("--provider", help="这个 instance 应该是谁（claude / codex），用来比对 Agent 自报身份")
    parser.add_argument("--ccnm", default=os.environ.get("CCNM_BIN", "ccnm"), help="ccnm 二进制")
    parser.add_argument("--config", default=os.environ.get("CCNM_CONFIG"), help="配置文件路径")
    parser.add_argument("--timeout", type=int, default=600, help="单条腿的秒数上限，默认 600")
    parser.add_argument("--out", type=Path, help="证据文件写到哪；不给就只打印")
    parser.add_argument("--keep-artifacts", action="store_true", help="跑完不删两个产物文件")
    parser.add_argument(
        "--guard-dir", type=Path,
        help="Runtime 执行身份的 write-guards/ 目录；给了就能观察 guard 何时放开，不给只能盲等",
    )
    parser.add_argument(
        "--settle-seconds", type=float, default=10.0,
        help="两条腿之间等 guard 放开的上限秒数，默认 10",
    )
    args = parser.parse_args()

    root: Path = args.root.expanduser().resolve()
    if not root.is_dir():
        raise SystemExit(f"工作树不存在或不是目录：{root}")

    # 开跑前就有 guard 锁着，说明有别的会话还占着工作树或者上一轮留了残留。
    # 这时候起腿注定失败，而失败要花一次额度——先停下来。
    preexisting = guards_held(args.guard_dir)
    if preexisting:
        raise SystemExit(
            "开跑前已经有工作树被占着，先按运维手册的写入 guard 残留一节处理：\n  "
            + "\n  ".join(preexisting)
        )

    token = "ccnmp7-" + secrets.token_hex(6)
    targets = {leg: root / f"ccnm-parity-{leg}-{token}.txt" for leg in ("cli", "api")}
    for path in targets.values():
        clear_target(path)

    print(f"token {token}")
    print(f"工作树 {root}")

    print("→ 第一条腿：人类 CLI（ccnm run --print）")
    human = run_human_cli(
        args.ccnm, args.config, args.workspace, args.instance,
        probe_prompt(targets["cli"], token), args.timeout,
    )
    human_effect = check_side_effect(targets["cli"], token)
    print(f"  {'ok' if human['ok'] else '失败'}：{human.get('reason') or '退出码 0'}"
          f" / 产物 {'对' if human_effect['matches'] else '不对'}")

    sequencing = settle_between_legs(args.guard_dir, args.settle_seconds)
    print(f"  等 guard 放开：{sequencing['method']} {sequencing['waited_s']}s"
          + ("（仍被占着）" if sequencing.get("still_held") else ""))

    print("→ 第二条腿：machine API（ccnm rpc）")
    machine = run_machine_api(
        args.ccnm, args.config, args.workspace, args.node, args.instance,
        probe_prompt(targets["api"], token), token, args.timeout,
    )
    machine_effect = check_side_effect(targets["api"], token)
    print(f"  {'ok' if machine['ok'] else '失败'}：{machine.get('reason') or 'completed'}"
          f" / 产物 {'对' if machine_effect['matches'] else '不对'}"
          f" / usage {machine.get('usage') or '无'} cost {machine.get('cost') or '无'}")

    leaked = scan_for_private(machine["responses"], str(Path.home()))
    checks = build_checks(human, human_effect, machine, machine_effect, leaked, args.provider)
    outcome = verdict(checks)
    sequencing["blamed_for_failure"] = blames_the_guard(machine)

    removed = []
    if not args.keep_artifacts:
        for leg, path in targets.items():
            if remove_artifact(path, token):
                removed.append(f"{leg}:{path.name}")

    evidence = {
        "kind": "p7-parity",
        "token": token,
        "workspace": args.workspace,
        "instance": args.instance,
        "declared_provider": args.provider,
        "workspace_root": str(root),
        "verdict": outcome,
        "checks": checks,
        "human_cli": human,
        "human_cli_side_effect": human_effect,
        "machine_api": {key: value for key, value in machine.items() if key != "responses"},
        "machine_api_side_effect": machine_effect,
        "private_markers_found": leaked,
        "artifacts_removed": removed,
        "sequencing": sequencing,
    }
    if args.out:
        args.out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"证据写到 {args.out}")

    print(f"\n判定：{outcome}")
    for check in checks:
        mark = {True: "通过", False: "未通过", None: "判不出"}[check["passed"]]
        print(f"  [{mark}] {check['name']}：{check['detail']}")
    if sequencing["blamed_for_failure"]:
        print("\n第二条腿是被工作树写入 guard 挡下的，不是协议问题：第一条腿的 Runtime MCP\n"
              "还没退干净。加大 --settle-seconds，或用 --guard-dir 指到 Runtime 执行身份的\n"
              "write-guards/ 让它等到真放开，然后重跑——这一条重跑是值得的。")
    if outcome != "pass":
        print("\n没通过的项要在记录里如实写明，不要重跑到绿为止——真机每一轮都在花额度。")
    return 0 if outcome == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
