#!/usr/bin/env python3
"""P113：经 `ccnm mcp bridge` 在装了 WSL 的 Windows 上看 `cmd` 里不带路径的 `bash` 跑的是哪个，
以及整个会话里 WSL 有没有被拉起来。

    python3 -I p113-windows-bare-bash.py --bridge "ccnm --config X mcp bridge p113 --mode coding" \
        --win ./w --out 证据.json

--win 是一个 `<脚本> <PowerShell 命令>` 形状的 ssh 包装，用来在 Windows 上查进程。客户端用仓库里的
tests/mcp_client.py，不 import 任何 ccnm 的东西。别在这里跑 `System32\\bash.exe` 的全路径：那会真的
起 WSL，验收机上 WSL 里的 mise 会顺手装东西（P112.2）。
"""
from __future__ import annotations

import argparse
import base64
import json
import shlex
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "tests"))
from mcp_client import McpClient, is_error, result_text  # noqa: E402

WSL_NAMES = ("wsl.exe", "wslhost.exe", "wslrelay.exe")


def windows(win: str, script: str) -> str:
    encoded = base64.b64encode(script.encode("utf-16-le")).decode()
    done = subprocess.run([win, f"powershell -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}"],
                          capture_output=True, text=True, timeout=120)
    return done.stdout


def windows_now(win: str) -> str:
    return windows(win, "(Get-Date).ToUniversalTime().ToString('o')").strip()


def wsl_started_since(win: str, start: str) -> list[str]:
    """`start` 之后起的 WSL 进程（客户端、宿主、转发）：`名字|创建时间`。"""
    names = " OR ".join(f"Name='{n}'" for n in WSL_NAMES)
    said = windows(win, (
        "Import-Module CimCmdlets; Get-CimInstance Win32_Process -Filter \"" + names + "\" | "
        "ForEach-Object { '{0}|{1}' -f $_.Name, $_.CreationDate.ToUniversalTime().ToString('o') }"))
    return [line.strip() for line in said.splitlines()
            if "|" in line and line.strip().split("|", 1)[1] >= start]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--bridge", required=True)
    parser.add_argument("--win", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()

    results = []

    def record(name: str, ok: bool, detail: str):
        results.append({"check": name, "ok": ok, "detail": detail[:1500]})
        print(f"{'PASS' if ok else 'FAIL'} {name}: {detail[:400]}", flush=True)

    start = windows_now(args.win)
    client = McpClient(shlex.split(args.bridge))
    client.initialize()

    def run(argv: list[str] | None = None, shell: str | None = None) -> str:
        got = client.call_tool("exec_command", {"cmd": argv} if argv else {"shell": shell})
        if is_error(got):
            raise RuntimeError(result_text(got))
        return result_text(got)

    said = run(["bash", "-c", "echo \"v=$BASH_VERSION\"; echo \"shell=$SHELL\"; uname -s"])
    # Inside Git Bash `$SHELL` reads as an MSYS path (`/d/...`); the Windows
    # path to run it by is the one the note gives.
    shell_line = next((line for line in said.splitlines() if line.startswith("shell=")), "")
    record("bare bash runs Git Bash", "MINGW" in said and shell_line.lower().endswith("bash.exe"), said)
    marker = "bash ran as Git for Windows' bash ("
    git_bash = said.split(marker, 1)[1].split(")", 1)[0] if marker in said else ""
    record("the note names WSL's bash", bool(git_bash)
           and "system32/bash.exe, which starts wsl" in said.lower(), said)

    said = run(["BASH.EXE", "--version"])
    record("BASH.EXE is the same", "pc-msys" in said and "which starts WSL" in said, said)

    said = run([git_bash, "-c", "uname -s"])
    record("a full path is left alone", "MINGW" in said and "bash ran as" not in said, said)

    said = run(shell="uname -s")
    record("shell is unchanged", "MINGW" in said and "bash ran as" not in said, said)

    client.close()
    started = wsl_started_since(args.win, start)
    record("WSL was not started", not started, json.dumps(started))

    failed = sum(1 for r in results if not r["ok"])
    args.out.write_text(json.dumps({"start": start, "results": results}, ensure_ascii=False, indent=2) + "\n",
                        encoding="utf-8")
    print(f"{len(results) - failed}/{len(results)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
