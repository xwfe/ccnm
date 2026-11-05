#!/usr/bin/env python3
"""P120：钩子能不能直接对人说一句话（零额度）。

要量的（真实 Claude Code 接本机假模型，做法照 p100-session-hooks.py）：
  1. SessionStart（startup / compact / clear）和 Stop 钩子往 stdout 打一个 JSON，
     `systemMessage` 会不会出现在界面上（tmux capture-pane 抓得到）；
  2. SessionStart 的 `hookSpecificOutput.additionalContext` 有没有进下一次发给模型的请求；
  3. 钩子 stdin 里有没有 `transcript_path`，指向哪段对话的记录。

P121 起 ccnm 给人的行带彩色的 `ccnm` 标签，最后一步另用 `capture-pane -e` 看颜色码在不在。

加 `--ccnm <ccnm 二进制>` 时另跑一轮：真的 `ccnm internal session-hook` 挂在 SessionStart 和
Stop 上（会话目录是临时的，HOME 也是，所以语言按默认的中文），看 P120 的几件事：第 1、2 次
压缩界面上没有提醒、第 3 次有；给模型的话里没有让它建议换新对话；写了交接时界面上有一行、
`/clear` 后交接进了第一条请求；没写交接、压缩够 3 次时 `/clear` 带的是上一段对话的尾巴。

用法（macOS，要 tmux 和 sandbox-exec）：
  python3 docs/research/probes/p120-hook-notices.py <仓库外的新输出目录> \\
      --claude cc=/path/to/claude [--ccnm target/debug/ccnm]
"""

import argparse
import importlib.util
import json
import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
P100 = HERE / "p100-session-hooks.py"
spec = importlib.util.spec_from_file_location("p100", P100)
p100 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p100)

HOOK = r'''
import json, sys
event = sys.argv[1]
raw = sys.stdin.read()
try:
    data = json.loads(raw)
except ValueError:
    data = {"unparsed": raw[:500]}
with open(sys.argv[2], "a") as log:
    log.write(json.dumps({"event": event, "input": data}) + "\n")
source = data.get("source") or ""
out = {"systemMessage": f"P120-NOTICE-{event}-{source}"}
if event == "SessionStart":
    out["hookSpecificOutput"] = {"hookEventName": "SessionStart",
                                 "additionalContext": f"P120-CTX-{source}"}
print(json.dumps(out))
'''


class NoticeProbe(p100.Probe):
    def __init__(self, out, claude):
        super().__init__(out, claude)
        (out / "hook.py").write_text(HOOK)
        doc = json.loads((out / "settings.json").read_text())
        doc["hooks"] = {e: doc["hooks"][e] for e in ["SessionStart", "Stop"]}
        (out / "settings.json").write_text(json.dumps(doc))

    def say(self, name, text, timeout=60):
        before = len(self.model.requests)
        self.keys(name, text)
        self.wait_requests(before, timeout)
        return before

    def screen(self, name, label):
        text = subprocess.run(self.tmux + ["capture-pane", "-p", "-S", "-200", "-t", name],
                              capture_output=True, text=True).stdout
        (self.out / f"pane-{label}.txt").write_text(text)
        return text

    def run(self):
        s1 = str(uuid.uuid4())
        name = "probe"
        try:
            self.start(name, ["--session-id", s1])
            startup = self.screen(name, "1-startup")
            since = self.say(name, "hello one")
            after_first = self.screen(name, "2-after-first-turn")
            self.step("startup",
                      notice_on_screen="P120-NOTICE-SessionStart-startup" in startup,
                      ctx_in_requests=self.requests_with("P120-CTX-startup", since),
                      stop_notice_on_screen="P120-NOTICE-Stop" in after_first)
            self.say(name, "/compact")
            time.sleep(3)
            compacted = self.screen(name, "3-after-compact")
            since = self.say(name, "after compact")
            self.step("compact",
                      notice_on_screen="P120-NOTICE-SessionStart-compact" in compacted,
                      ctx_in_requests=self.requests_with("P120-CTX-compact", since))
            self.say(name, "/clear", 10)
            time.sleep(3)
            cleared = self.screen(name, "4-after-clear")
            since = self.say(name, "after clear")
            self.step("clear",
                      notice_on_screen="P120-NOTICE-SessionStart-clear" in cleared,
                      ctx_in_requests=self.requests_with("P120-CTX-clear", since))
            hooks = self.hooks()
            self.step("inputs", fields={f"{h['event']}-{h['input'].get('source', '')}":
                                        sorted(h["input"].keys()) for h in hooks},
                      transcripts=[(h["event"], h["input"].get("source"), h["input"].get("session_id"),
                                    h["input"].get("transcript_path")) for h in hooks])
            self.keys(name, "/exit")
            time.sleep(3)
        finally:
            subprocess.run(self.tmux + ["kill-server"], capture_output=True)
            try:
                os.unlink(self.sock)
            except OSError:
                pass
            self.model.server.shutdown()
        return {"session": s1, "steps": self.steps}


class CcnmNoticeProbe(NoticeProbe):
    """The same Claude Code, with ccnm's own session hook beside the logger."""

    def __init__(self, out, claude, ccnm):
        super().__init__(out, claude)
        self.session_dir = out / "session"
        self.session_dir.mkdir()
        doc = json.loads((out / "settings.json").read_text())
        for event in ["SessionStart", "Stop"]:
            doc["hooks"][event].append(
                {"hooks": [{"type": "command", "command": p100.ccnm_hook(ccnm, self.session_dir)}]})
        (out / "settings.json").write_text(json.dumps(doc))

    def ccnm_says(self, text, event):
        return [line.strip() for line in text.splitlines()
                if f"{event} says:" in line and "ccnm" in line]

    def coloured(self, name):
        """P121: the tags as drawn, from a capture that keeps colour codes."""
        raw = subprocess.run(self.tmux + ["capture-pane", "-p", "-e", "-S", "-200", "-t", name],
                             capture_output=True, text=True).stdout
        return {"yellow_tag": "\x1b[1;30;43m ccnm" in raw or "\x1b[1m\x1b[30m\x1b[43m ccnm" in raw,
                "green_tag": "\x1b[1;30;42m ccnm" in raw or "\x1b[1m\x1b[30m\x1b[42m ccnm" in raw}

    def run(self):
        s1 = str(uuid.uuid4())
        name = "probe"
        nag = "When the task the user gave you is finished"
        try:
            self.start(name, ["--session-id", s1])
            self.say(name, "hello one")
            for n in [1, 2, 3]:
                self.say(name, "/compact")
                time.sleep(3)
                screen = self.screen(name, f"c{n}-after-compact")
                # /clear redraws the screen, so the colours are looked at here.
                colours = self.coloured(name) if n == 3 else None
                since = self.say(name, f"after compact {n}")
                self.step(f"compact-{n}", ccnm_line=self.ccnm_says(screen, "SessionStart:compact"),
                          nag_in_requests=self.requests_with(nag, since),
                          count_in_requests=self.requests_with(f"compacted {'once' if n == 1 else f'{n} times'}", since),
                          colours=colours)
            # Cleared with no handoff after three compactions: the end goes.
            self.say(name, "/clear", 10)
            time.sleep(3)
            screen = self.screen(name, "c4-after-clear-no-handoff")
            since = self.say(name, "first after clear")
            self.step("clear-no-handoff", ccnm_line=self.ccnm_says(screen, "SessionStart:clear"),
                      tail_in_requests=self.requests_with("raw, not a summary", since),
                      earlier_words_in_requests=self.requests_with("after compact 3", since))
            # Asked for a handoff: kept, announced, carried on /clear.
            self.say(name, "write the HANDOFF now")
            screen = self.screen(name, "c5-after-handoff")
            self.step("handoff", ccnm_line=self.ccnm_says(screen, "Stop"),
                      kept=(self.session_dir / "handoff.md").exists())
            self.say(name, "/clear", 10)
            time.sleep(3)
            screen = self.screen(name, "c6-after-clear-handoff")
            since = self.say(name, "first after the second clear")
            self.step("clear-handoff", ccnm_line=self.ccnm_says(screen, "SessionStart:clear"),
                      handoff_in_requests=self.requests_with("Goal: P100 probe", since),
                      colours=self.coloured(name))
            self.keys(name, "/exit")
            time.sleep(3)
        finally:
            subprocess.run(self.tmux + ["kill-server"], capture_output=True)
            try:
                os.unlink(self.sock)
            except OSError:
                pass
            self.model.server.shutdown()
        return {"session": s1, "steps": self.steps}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("out")
    parser.add_argument("--claude", required=True, help="name=path")
    parser.add_argument("--ccnm", help="a ccnm binary: also run its session hook in real Claude Code")
    args = parser.parse_args()
    out = Path(args.out).resolve()
    out.mkdir(parents=True)
    name, path = args.claude.split("=", 1)
    version = subprocess.run([path, "--version"], capture_output=True, text=True).stdout.strip()
    summary = {name: {"version": version, **NoticeProbe(out / name, path).run()}}
    if args.ccnm:
        ccnm = str(Path(args.ccnm).resolve())
        summary[f"{name}-ccnm"] = {"version": version,
                                   **CcnmNoticeProbe(out / f"{name}-ccnm", path, ccnm).run()}
    (out / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
