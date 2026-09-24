"""P11：用一个与 ccnm 无关的 MCP 客户端重放同一套允许矩阵。

Rust 那边的 `crates/ccnm-cli/tests/external_mcp.rs` 已经测过同样的事。这里
再来一遍不是重复：那边的客户端和被测的服务端在同一个仓库、同一种语言、共用
同一批类型；这里的客户端只知道"一行一条 JSON-RPC"，连 payload 都是自己拼
的。两个独立实现得出同一个结论，才说明结论属于协议而不属于某段代码。

**这里没有真实 MCP Host。** Claude Code / Codex 的验证是 P11.3 剩下的那半
边，需要真机和订阅额度，status.json 里记着 blocker。
"""

# macOS 自带 Python 3.9：没有这一行，下面的 `dict | None` 注解会在 import 时
# 就抛 TypeError。
from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(Path(__file__).resolve().parent))

from mcp_client import McpClient, RpcError, is_error, result_text  # noqa: E402


def ccnm_binary() -> Path | None:
    override = os.environ.get("CCNM_BIN")
    if override:
        return Path(override)
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / "ccnm"
        if candidate.is_file():
            return candidate
    return None


BINARY = ccnm_binary()

# read 模式该有的全部工具，以及四个不该有的。load_skill（P36）、view_image
# （P39）、read_notebook（P40）只读、什么都不执行，所以 read 模式也有。
READ_TOOLS = [
    "list_files", "load_skill", "read_file", "read_notebook", "search_text", "view_image", "workspace_info",
]

DEPLOY_SKILL = """---
name: deploy
description: >
  Deploy the service.
  Use after tests pass.
arguments: [env]
allowed-tools: Bash(git *)
---

# Deploy

Target: $env. Status first: !`touch ran-by-loading`
Run ${CLAUDE_SKILL_DIR}/scripts/go.sh
"""
WITHHELD = {
    "exec_command": {"cmd": ["/bin/echo", "hi"]},
    "apply_patch": {"files": [{"op": "add", "path": "sneaked.txt", "content": "x\n"}]},
    "read_output": {"output_ref": "r-0000000000000000"},
    "stop_command": {"output_ref": "r-0000000000000000"},
}


def still_running(pid: int) -> bool:
    """还能运行：既不是没了，也不是只等着被收的僵尸。"""
    state = subprocess.run(
        ["/bin/ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True, check=False,
    ).stdout.strip()
    return bool(state) and not state.startswith("Z")


def stop_if_still_ours(pid: int, marker: str) -> None:
    """测试失败时收掉本轮起的进程：命令行里有本轮的临时路径才动，pid 换了人就不碰。"""
    found = subprocess.run(
        ["/bin/ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True, check=False,
    ).stdout
    if marker in found:
        os.kill(pid, 9)


def external_payload(workspace: str, session: str, mode: str) -> str:
    """internal 协议 5 的 payload，由这个测试自己拼。

    真实部署里拼它的是 `ccnm mcp bridge`；这里手拼是为了不经过 ccnm 的任何
    代码，也顺带证明这个形状简单到别的实现照着文档就能写。
    """
    body = json.dumps(
        {"protocol": 5, "workspace": workspace, "session": session, "mode": mode},
        ensure_ascii=False,
    ).encode("utf-8")
    return base64.urlsafe_b64encode(body).decode("ascii").rstrip("=")


@unittest.skipIf(BINARY is None, "先 cargo build，或用 CCNM_BIN 指定二进制")
class RemoteWorkspaceMcpTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ccnm-neutral-")
        self.addCleanup(self.temp.cleanup)
        # 解析成真实路径。macOS 的 /var 是指向 /private/var 的符号链接，而
        # 凭据检查对"路径可达性未知"是 fail-closed 的：HOME 藏在符号链接
        # 后面时它报 unknown，服务端于是拒绝启动。那是它该有的谨慎，不是这
        # 里要测的东西。
        self.dir = Path(self.temp.name).resolve()
        self.root = self.dir / "project"
        for sub in ("project", "other", "home", "state"):
            (self.dir / sub).mkdir()
        (self.root / "hello.txt").write_text("one\ntwo\n", encoding="utf-8")
        self.config = self.dir / "config.toml"
        self.write_config("read")

    def write_config(self, access: str, unconfined: bool = False) -> None:
        # unconfined：这台测试机不是隔离的执行身份，不写它 exec_command 一律
        # 被执行门拒绝，参数根本到不了检查那一步。
        self.config.write_text(
            f"""
this = "runtime"

[nodes.runtime]

[nodes.agent]
ssh = "agent-node.invalid"

[workspaces.demo]
root = "{self.root}"
agent = {{ node = "agent", instance = "claude-main" }}
external_mcp = "{access}"
allow_unconfined_exec = {"true" if unconfined else "false"}

[workspaces.private]
root = "{self.dir / "other"}"
agent_node = "agent"
""",
            encoding="utf-8",
        )

    def env(self) -> dict:
        return {
            "PATH": os.environ.get("PATH", ""),
            "HOME": str(self.dir / "home"),
            "XDG_STATE_HOME": str(self.dir / "state"),
            "CCNM_CONFIG": str(self.config),
        }

    def argv(self, workspace: str, mode: str, session: str) -> list:
        return [
            str(BINARY),
            "internal",
            "mcp-serve",
            "--payload",
            external_payload(workspace, session, mode),
        ]

    def client(self, workspace: str, mode: str, session: str) -> McpClient:
        client = McpClient(self.argv(workspace, mode, session), self.env())
        self.addCleanup(client.close)
        client.initialize()
        return client

    def refused(self, workspace: str, mode: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            self.argv(workspace, mode, "neutral-refused"),
            env=self.env(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )

    # -- 允许矩阵 --

    def test_a_read_session_offers_exactly_the_read_tools(self):
        client = self.client("demo", "read", "neutral-read")
        self.assertEqual(client.tool_names(), READ_TOOLS)

    def test_the_read_tools_actually_work(self):
        client = self.client("demo", "read", "neutral-works")
        got = client.call_tool("read_file", {"path": "hello.txt"})
        self.assertFalse(is_error(got), got)
        self.assertIn("one", result_text(got))

    def test_a_read_session_refuses_what_it_did_not_offer(self):
        # 一个不看 tools/list 的 Host：直接按名字调。参数都是合法的，否则被
        # 拦下的会是参数检查，那什么都证明不了。
        client = self.client("demo", "read", "neutral-refuse")
        for tool, arguments in WITHHELD.items():
            with self.subTest(tool=tool):
                got = client.call_tool(tool, arguments)
                self.assertTrue(is_error(got), got)
                self.assertTrue(result_text(got).startswith("CCNM_E_POLICY:"), result_text(got))
        self.assertFalse((self.root / "sneaked.txt").exists(), "被拒的写不能真的发生")

    def test_a_coding_session_gets_every_tool(self):
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-coding")
        self.assertEqual(len(client.tool_names()), len(READ_TOOLS) + len(WITHHELD))
        for tool in WITHHELD:
            self.assertIn(tool, client.tool_names())

    # -- 项目自带的 skills（P36） --

    def add_skill(self, name: str = "deploy", text: str = DEPLOY_SKILL) -> None:
        path = self.root / ".claude" / "skills" / name / "SKILL.md"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def skill_tool(self, client: McpClient) -> dict:
        return next(tool for tool in client.tools() if tool["name"] == "load_skill")

    def test_the_catalog_rides_in_the_tool_description(self):
        # 没有 skill 时 description 是固定文本；有了之后，名字和（折成一行的）
        # 描述出现在里面——模型整个会话里一直看得见的就是这一段。
        bare = self.skill_tool(self.client("demo", "read", "neutral-skill-none"))
        self.assertTrue(bare["description"].endswith("This workspace has no skills right now."))
        self.add_skill()
        listed = self.skill_tool(self.client("demo", "read", "neutral-skill-one"))
        self.assertIn("- deploy (arguments: env): Deploy the service. Use after tests pass.",
                      listed["description"])
        # Claude Code 2.1.273 只留每个工具 description 的前 2048 个 UTF-16 码元。
        self.assertLessEqual(len(listed["description"].encode("utf-16-le")) // 2, 2048)
        self.assertTrue(listed["annotations"]["readOnlyHint"])

    def test_loading_a_skill_fills_arguments_and_runs_nothing(self):
        self.add_skill()
        client = self.client("demo", "read", "neutral-skill-load")
        got = client.call_tool("load_skill", {"name": "deploy", "arguments": "staging"})
        self.assertFalse(is_error(got), got)
        text = result_text(got)
        self.assertIn("Target: staging.", text)
        self.assertIn("Run .claude/skills/deploy/scripts/go.sh", text)
        self.assertIn("were NOT run", text)
        self.assertIn("allowed-tools", text)
        self.assertNotIn("name: deploy", text, "frontmatter 不是给模型的指令")
        # 原生客户端会在加载时执行 !`命令`；这里一次"读"不能变成一次"执行"。
        self.assertFalse((self.root / "ran-by-loading").exists())

    def test_without_a_name_the_whole_list_comes_back(self):
        self.add_skill()
        # 引号不闭合：原生客户端和这里都读不了。P45 之前这里用的是
        # `description: &anchor x`，宿主读得了，0.2.0 的共享库也就读得了。
        self.add_skill("broken", "---\nname: broken\ndescription: \"open\n---\nbody\n")
        client = self.client("demo", "read", "neutral-skill-list")
        text = result_text(client.call_tool("load_skill", {}))
        self.assertIn("- deploy", text)
        # 读不了的 skill 不是悄悄消失，而是说出文件和原因。
        self.assertIn(".claude/skills/broken/SKILL.md", text)
        self.assertIn("frontmatter line 2", text)

    def test_an_unknown_skill_is_an_error_the_model_can_act_on(self):
        self.add_skill()
        client = self.client("demo", "read", "neutral-skill-unknown")
        got = client.call_tool("load_skill", {"name": "deplyo"})
        self.assertTrue(is_error(got), got)
        self.assertTrue(result_text(got).startswith("CCNM_E_INVALID_ARGS:"), result_text(got))
        self.assertIn("deploy", result_text(got))

    def test_skills_are_also_prompts_for_a_person_to_start(self):
        self.add_skill()
        self.add_skill("hidden", "---\ndescription: Background.\nuser-invocable: false\n---\nx\n")
        client = self.client("demo", "read", "neutral-skill-prompts")
        prompts = {p["name"]: p for p in client.call("prompts/list", {})["prompts"]}
        self.assertEqual(sorted(prompts), ["deploy"])
        self.assertEqual([a["name"] for a in prompts["deploy"]["arguments"]], ["env"])
        got = client.call("prompts/get", {"name": "deploy", "arguments": {"env": "prod"}})
        message = got["messages"][0]
        self.assertEqual(message["role"], "user")
        self.assertIn("Target: prod.", message["content"]["text"])
        with self.assertRaises(RpcError):
            client.call("prompts/get", {"name": "hidden", "arguments": {}})

    # -- 这台机器上装好的 skills（P48） --

    def add_installed(self, name: str, description: str, files: dict | None = None) -> Path:
        """执行账号 HOME 里装好的一个 skill，skills CLI 的放法。"""
        skill = self.dir / "home" / ".agents" / "skills" / name
        skill.mkdir(parents=True, exist_ok=True)
        (skill / "SKILL.md").write_text(
            f"---\ndescription: {description}\n---\nBody of {name}.\n", encoding="utf-8"
        )
        for rel, text in (files or {}).items():
            path = skill / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return skill

    def test_installed_skills_come_after_the_projects_and_their_files_can_be_read(self):
        self.add_skill()
        self.add_installed("pdf", "Fill PDF forms.", {"reference.md": "# Fields\n", ".env": "TOKEN=x\n"})
        client = self.client("demo", "read", "neutral-installed")
        text = self.skill_tool(client)["description"]
        # 项目的在前：2048 个码元不够时，先挤掉的是装在机器上的。
        self.assertLess(text.index("Skills in this workspace:"), text.index("Installed on this machine:"))
        self.assertIn("- pdf: Fill PDF forms.", text)
        loaded = result_text(client.call_tool("load_skill", {"name": "pdf"}))
        self.assertIn("[other files in this skill's directory: reference.md;", loaded)
        got = client.call_tool("load_skill", {"name": "pdf", "file": "reference.md"})
        self.assertFalse(is_error(got), got)
        self.assertIn("# Fields", result_text(got))
        # read 模式没有 exec_command，read_file 又只读工作区：这是读到工作区外面的
        # 唯一一条路，所以点文件和 skill 目录以外一律不给。
        for name in (".env", "../../../.bashrc"):
            refused = client.call_tool("load_skill", {"name": "pdf", "file": name})
            self.assertTrue(is_error(refused), refused)
            self.assertTrue(result_text(refused).startswith("CCNM_E_POLICY:"), result_text(refused))
            self.assertNotIn("TOKEN", result_text(refused))

    def test_the_machine_decides_whether_and_which_installed_skills_are_shared(self):
        self.add_installed("pdf", "Fill PDF forms.")
        self.add_installed("noise", "Something every session can do without.")
        base = self.config.read_text(encoding="utf-8")
        self.config.write_text(base + '\n[machine_skills]\nhidden = ["noise"]\n', encoding="utf-8")
        text = self.skill_tool(self.client("demo", "read", "neutral-hidden"))["description"]
        self.assertIn("- pdf:", text)
        self.assertNotIn("noise", text)
        self.config.write_text(base + "\n[machine_skills]\nenabled = false\n", encoding="utf-8")
        text = self.skill_tool(self.client("demo", "read", "neutral-unshared"))["description"]
        self.assertTrue(text.endswith("This workspace has no skills right now."), text)

    # -- 项目那台机器上的 MCP server（P49） --

    def declare_server(self) -> None:
        """项目的 .mcp.json 里声明一个 stdio server：配置里给它一个 token，
        还（错误地）写了一个 Agent 的登录变量——后者必须到不了它。"""
        server = ROOT / "tests" / "fixtures" / "fake_mcp_server.py"
        (self.root / ".mcp.json").write_text(json.dumps({"mcpServers": {
            "fake": {
                "command": sys.executable,
                "args": [str(server)],
                "env": {"DB_TOKEN": "t0k", "ANTHROPIC_API_KEY": "must-not-arrive"},
            },
            "web": {"type": "http", "url": "https://example.invalid/mcp"},
        }}), encoding="utf-8")

    def relay_tool(self, client: McpClient) -> dict | None:
        return next((tool for tool in client.tools() if tool["name"] == "call_mcp_tool"), None)

    def test_a_projects_mcp_server_is_relayed_to_a_coding_session(self):
        self.declare_server()
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-relay")
        tool = self.relay_tool(client)
        self.assertIsNotNone(tool, client.tool_names())
        self.assertTrue(tool["description"].endswith("Servers here: fake."), tool["description"])
        self.assertFalse(tool["annotations"]["readOnlyHint"])
        self.assertTrue(tool["annotations"]["openWorldHint"])

        overview = result_text(client.call_tool("call_mcp_tool", {}))
        self.assertIn("- fake (.mcp.json): not started", overview)
        self.assertIn("- web (.mcp.json): not relayed: it is an HTTP server", overview)

        listed = result_text(client.call_tool("call_mcp_tool", {"server": "fake"}))
        self.assertTrue(listed.startswith("[MCP server fake (fake 1.0): 4 tool(s)]"), listed)
        self.assertIn("Keep calls small.", listed)

        echoed = client.call_tool("call_mcp_tool", {"server": "fake", "tool": "echo", "arguments": {"q": 1}})
        self.assertFalse(is_error(echoed), echoed)
        # 一份文字，不带那份一样的 structuredContent。
        self.assertEqual(result_text(echoed), '{"q": 1}')
        self.assertNotIn("structuredContent", echoed)

        whoami = result_text(client.call_tool("call_mcp_tool", {"server": "fake", "tool": "whoami"}))
        self.assertEqual(whoami, "token=t0k agent=")

    def test_a_long_result_continues_with_read_output(self):
        self.declare_server()
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-relay-long")
        got = client.call_tool("call_mcp_tool", {"server": "fake", "tool": "big"})
        blocks = [block["text"] for block in got["content"]]
        self.assertLessEqual(len(blocks[0].encode()), 32768)
        note = blocks[1]
        self.assertIn("read_output output_ref=", note)
        reference = note.split("output_ref=")[1].split()[0]
        offset = int(note.split("offset=")[1].rstrip("]").split()[0])
        whole = blocks[0].encode()
        while True:
            page = client.call_tool("read_output", {"output_ref": reference, "offset": offset, "limit": 32768})
            self.assertFalse(is_error(page), page)
            # 正文之后一行页脚，写着这一页读到第几个字节（正文不以换行结尾时，
            # 页脚前会多一个换行，所以按页脚的数字取正文）。
            text = result_text(page)
            at = text.rfind("\n[")
            body, footer = text[: at + 1], text[at + 2:]
            if footer.startswith("end of stdout at "):
                upto = int(footer.split(" at ")[1].split()[0])
            else:
                upto = int(footer.split("offset=")[1].rstrip("]"))
            whole += body.encode()[: upto - offset]
            offset = upto
            if footer.startswith("end of stdout"):
                break
        self.assertEqual(len(whole), 52000, "分段读完一个字节不少")
        whole = whole.decode()
        self.assertIn("line 00999", whole)

    def test_ending_the_session_stops_the_servers_it_started(self):
        self.declare_server()
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-relay-end")
        pid = int(result_text(client.call_tool("call_mcp_tool", {"server": "fake", "tool": "pid"})))
        os.kill(pid, 0)  # 活着
        self.assertEqual(client.close(), 0)
        # server 能写工作树，所以它得在写锁放掉之前走：ccnm 退出时它已经没了。
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        # 写锁确实放了：下一个 coding 会话进得来。
        self.client("demo", "coding", "neutral-relay-next")

    def test_a_child_left_in_the_servers_process_group_ends_before_the_next_writer(self):
        """C51-01：P51 探针（docs/research/probes/p51-relay-cleanup.py）转成的回归。

        server 起一个子进程：同一个进程组、没有 setsid、关掉继承的管道、一直
        写文件；然后 server 读到 EOF 正常退出。P52 之前 ccnm 把"server 自己退
        了"当成清理完，写锁标 released，第二个 writer 进来时旧子进程还在写。
        """
        self.write_config("coding", unconfined=True)
        tick = self.root / "child-tick"
        child_file = self.root / "child.pid"
        child_code = (
            "import time; from pathlib import Path; "
            f"p=Path({str(tick)!r}); "
            "[(p.write_text(str(i)),time.sleep(0.1)) for i in range(600)]"
        )
        server = self.root / "leaving-server.py"
        server.write_text(
            "import subprocess,sys,runpy; from pathlib import Path\n"
            f"p=subprocess.Popen([sys.executable,'-c',{child_code!r}],"
            "stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)\n"
            f"Path({str(child_file)!r}).write_text(str(p.pid))\n"
            f"runpy.run_path({str(ROOT / 'tests/fixtures/fake_mcp_server.py')!r},run_name='__main__')\n",
            encoding="utf-8",
        )
        (self.root / ".mcp.json").write_text(json.dumps({"mcpServers": {
            "leaving": {"command": sys.executable, "args": [str(server)]},
        }}), encoding="utf-8")

        first = self.client("demo", "coding", "neutral-relay-group")
        got = first.call_tool("call_mcp_tool", {"server": "leaving", "tool": "pid"})
        self.assertFalse(is_error(got), got)
        server_pid = int(result_text(got))
        child = int(child_file.read_text())
        self.addCleanup(stop_if_still_ours, child, str(tick))
        self.assertEqual(os.getpgid(child), os.getpgid(server_pid), "同一个进程组")
        self.assertTrue(still_running(child))

        self.assertEqual(first.close(), 0)
        self.assertFalse(still_running(child), "server 组里剩下的在交出写锁前没了")
        before = tick.read_text(encoding="utf-8")
        time.sleep(0.4)
        self.assertEqual(tick.read_text(encoding="utf-8"), before, "关掉之后不再写")
        markers = [p.read_text(encoding="utf-8") for p in (self.dir / "state").rglob("*.lock")]
        self.assertEqual(markers, ["released\n"])

        second = self.client("demo", "coding", "neutral-relay-group-next")
        changed = second.call_tool("apply_patch", {"files": [
            {"op": "add", "path": "second-writer.txt", "content": "second writer\n"},
        ]})
        self.assertFalse(is_error(changed), changed)

    def test_a_read_session_never_gets_the_relay(self):
        self.declare_server()
        client = self.client("demo", "read", "neutral-relay-read")
        self.assertIsNone(self.relay_tool(client))
        refused = client.call_tool("call_mcp_tool", {"server": "fake", "tool": "echo"})
        self.assertTrue(result_text(refused).startswith("CCNM_E_POLICY:"), result_text(refused))

    def test_an_unconfined_runtime_without_the_opt_in_starts_no_server(self):
        self.declare_server()
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-relay-gate")
        # 只是列一下：什么都不起，不用过执行门。
        self.assertFalse(is_error(client.call_tool("call_mcp_tool", {})))
        refused = client.call_tool("call_mcp_tool", {"server": "fake"})
        self.assertTrue(result_text(refused).startswith("CCNM_E_POLICY:"), result_text(refused))
        self.assertIn("like exec_command", result_text(refused))

    def test_the_machine_decides_whether_mcp_servers_are_relayed(self):
        self.declare_server()
        self.write_config("coding", unconfined=True)
        base = self.config.read_text(encoding="utf-8")
        self.config.write_text(base + "\n[runtime_mcp]\nproject = false\n", encoding="utf-8")
        # coding 会话占着这个工作树的写锁，下一个会话得等它走。
        first = self.client("demo", "coding", "neutral-relay-noproject")
        self.assertIsNone(self.relay_tool(first))
        first.close()
        self.config.write_text(base + "\n[runtime_mcp]\nenabled = false\n", encoding="utf-8")
        self.assertIsNone(self.relay_tool(self.client("demo", "coding", "neutral-relay-off")))

    def test_annotations_say_what_the_runtime_enforces(self):
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-hints")
        hints = {tool["name"]: tool["annotations"] for tool in client.tools()}
        for tool in READ_TOOLS + ["read_output"]:
            self.assertIs(hints[tool]["readOnlyHint"], True, tool)
            self.assertIs(hints[tool]["openWorldHint"], False, tool)
        self.assertIs(hints["apply_patch"]["readOnlyHint"], False)
        self.assertIs(hints["apply_patch"]["destructiveHint"], True)
        # 只能改这个 workspace 里的文件。
        self.assertIs(hints["apply_patch"]["openWorldHint"], False)
        # 任意命令永远算 open-world，不看这次是什么命令。
        self.assertIs(hints["exec_command"]["openWorldHint"], True)
        self.assertIs(hints["exec_command"]["destructiveHint"], True)
        # 只停得到这个会话自己起的命令。
        self.assertIs(hints["stop_command"]["readOnlyHint"], False)
        self.assertIs(hints["stop_command"]["openWorldHint"], False)

    # -- 执行面第一批（P37）：搜索模式、整文件覆盖、一行 shell --

    def search_lines(self, client: McpClient, arguments: dict) -> list:
        got = client.call_tool("search_text", arguments)
        self.assertFalse(is_error(got), got)
        # 去掉方括号里的页脚和说明，剩下的是结果行；rg 不保证文件顺序。
        return sorted(line for line in result_text(got).splitlines() if not line.startswith("["))

    def test_search_text_lists_files_or_counts_them(self):
        (self.root / "src").mkdir()
        (self.root / "src" / "a.rs").write_text("needle\nneedle needle\n", encoding="utf-8")
        (self.root / "src" / "b.py").write_text("needle = 1\n", encoding="utf-8")
        client = self.client("demo", "read", "neutral-search-modes")
        self.assertEqual(
            self.search_lines(client, {"query": "needle", "output_mode": "files_with_matches"}),
            ["src/a.rs", "src/b.py"],
        )
        # 一行里两处算一行，和 rg --count 一样。
        self.assertEqual(
            self.search_lines(client, {"query": "needle", "output_mode": "count"}),
            ["src/a.rs:2", "src/b.py:1"],
        )
        self.assertEqual(
            self.search_lines(
                client, {"query": "needle", "type": "py", "output_mode": "files_with_matches"}
            ),
            ["src/b.py"],
        )
        # type 和 glob 一起给时两者都要满足（P37 曾经拒绝这种组合，P38 起不再）。
        self.assertEqual(
            self.search_lines(
                client,
                {"query": "needle", "type": "rust", "glob": "src/**", "output_mode": "files_with_matches"},
            ),
            ["src/a.rs"],
        )

    def test_a_glob_never_reaches_what_gitignore_rules_out(self):
        # P38：rg 的 --glob 一命中就不看 .gitignore；只能匹配文件的 *.yml 也一样。
        (self.root / ".git").mkdir()
        (self.root / ".gitignore").write_text("target/\nsecret.yml\n", encoding="utf-8")
        (self.root / "target").mkdir()
        (self.root / "target" / "out.rs").write_text("needle\n", encoding="utf-8")
        (self.root / "secret.yml").write_text("needle: 1\n", encoding="utf-8")
        (self.root / "app.yml").write_text("needle: 2\n", encoding="utf-8")
        client = self.client("demo", "read", "neutral-search-gitignore")
        base = {"query": "needle", "output_mode": "files_with_matches"}
        self.assertEqual(self.search_lines(client, {**base, "glob": "**"}), ["app.yml"])
        self.assertEqual(self.search_lines(client, {**base, "glob": "*.yml"}), ["app.yml"])

    def test_search_text_spans_lines_only_when_asked(self):
        (self.root / "call.rs").write_text("f(1,\n  2);\n", encoding="utf-8")
        client = self.client("demo", "read", "neutral-search-multiline")
        got = client.call_tool(
            "search_text",
            {"query": r"f\(1,.*?\);", "regex": True, "multiline": True, "context_lines": 0},
        )
        self.assertIn("call.rs\n1:f(1,\n2:  2);\n", result_text(got))
        without = client.call_tool("search_text", {"query": "f(1,\n  2);"})
        self.assertTrue(result_text(without).startswith("CCNM_E_INVALID_ARGS:"), result_text(without))

    def test_dotfiles_are_opt_in_and_git_is_never_searched(self):
        (self.root / ".env").write_text("needle=1\n", encoding="utf-8")
        (self.root / ".git").mkdir()
        (self.root / ".git" / "config").write_text("needle\n", encoding="utf-8")
        client = self.client("demo", "read", "neutral-search-hidden")
        base = {"query": "needle", "output_mode": "files_with_matches"}
        # 一个能匹配目录的 glob 以前会把 dotfile 带回来（P37 修掉）。
        for extra in ({}, {"glob": "**"}):
            with self.subTest(extra=extra):
                self.assertEqual(self.search_lines(client, {**base, **extra}), [])
                self.assertEqual(
                    self.search_lines(client, {**base, **extra, "include_hidden": True}), [".env"]
                )

    def version_of(self, client: McpClient, path: str) -> str:
        footer = result_text(client.call_tool("read_file", {"path": path})).rsplit("; version ", 1)
        self.assertEqual(len(footer), 2, footer)
        return footer[1].rstrip("]")

    def test_apply_patch_write_replaces_a_file_it_has_read(self):
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-write")
        stale = {"op": "write", "path": "hello.txt", "content": "whole\n", "version": "0-0"}
        got = client.call_tool("apply_patch", {"files": [stale]})
        self.assertTrue(result_text(got).startswith("CCNM_E_STALE_EPOCH:"), result_text(got))

        fresh = {**stale, "version": self.version_of(client, "hello.txt")}
        got = client.call_tool("apply_patch", {"files": [fresh]})
        self.assertFalse(is_error(got), got)
        self.assertTrue(result_text(got).startswith("write  hello.txt (8 -> 6 bytes)"), result_text(got))
        self.assertEqual((self.root / "hello.txt").read_text(encoding="utf-8"), "whole\n")

        # write 不新建文件：那是 add。
        new = {"op": "write", "path": "new.txt", "content": "x\n"}
        got = client.call_tool("apply_patch", {"files": [new]})
        self.assertIn('use op "add"', result_text(got))
        self.assertFalse((self.root / "new.txt").exists())

    def test_exec_command_takes_one_shell_line(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-shell")
        got = client.call_tool("exec_command", {"shell": "cat hello.txt | wc -l && echo done > out.txt"})
        self.assertFalse(is_error(got), got)
        text = result_text(got)
        self.assertTrue(text.startswith("$ cat hello.txt | wc -l && echo done > out.txt\nok in"), text)
        self.assertEqual((self.root / "out.txt").read_text(encoding="utf-8"), "done\n")

        for arguments in ({"cmd": ["true"], "shell": "true"}, {}):
            with self.subTest(arguments=arguments):
                got = client.call_tool("exec_command", arguments)
                self.assertTrue(result_text(got).startswith("CCNM_E_INVALID_ARGS:"), result_text(got))

    # -- view_image（P39） --

    def test_view_image_sends_the_file_as_an_image_block(self):
        png = bytes.fromhex(
            "89504e470d0a1a0a0000000d4948445200000002000000020802000000fdd49a73"
            "0000001049444154789c63f8cfc000440c100a001fee03fd8b5f14d40000000049454e44ae426082"
        )
        (self.root / "shots").mkdir()
        (self.root / "shots" / "red.png").write_bytes(png)
        (self.root / "shots" / "notes.txt").write_text("not an image\n", encoding="utf-8")
        client = self.client("demo", "read", "neutral-view-image")
        got = client.call_tool("view_image", {"path": "shots/red.png"})
        self.assertFalse(is_error(got), got)
        text, image = got["content"]
        self.assertEqual(text, {"type": "text", "text": f"shots/red.png: PNG, {len(png)} bytes"})
        # 两个 Host 都只把 image 块当图片（toexec evidence/v3-parity/media-surface）。
        self.assertEqual(image["type"], "image")
        self.assertEqual(image["mimeType"], "image/png")
        self.assertEqual(base64.b64decode(image["data"]), png, "原样发出，不缩放不转码")

        refused = client.call_tool("view_image", {"path": "shots/notes.txt"})
        self.assertTrue(result_text(refused).startswith("CCNM_E_INVALID_ARGS:"), refused)
        # read_file 看到图片时指向 view_image。
        pointed = client.call_tool("read_file", {"path": "shots/red.png"})
        self.assertIn("view_image", result_text(pointed))

    # -- notebook（P40） --

    NOTEBOOK = ROOT / "tests" / "fixtures" / "notebook" / "analysis.ipynb"

    def add_notebook(self) -> None:
        (self.root / "analysis.ipynb").write_bytes(self.NOTEBOOK.read_bytes())

    def test_read_notebook_shows_cells_outputs_and_images_in_order(self):
        self.add_notebook()
        client = self.client("demo", "read", "neutral-notebook-read")
        got = client.call_tool("read_notebook", {"path": "analysis.ipynb"})
        self.assertFalse(is_error(got), got)
        kinds = [block["type"] for block in got["content"]]
        self.assertEqual(kinds, ["text", "image", "text"])
        first, image, rest = got["content"]
        self.assertIn('<cell id="b7d3a901" index="1" type="code" execution_count="1">', first["text"])
        self.assertEqual(image["mimeType"], "image/png")
        self.assertTrue(base64.b64decode(image["data"]).startswith(b"\x89PNG"))
        self.assertIn("ZeroDivisionError: division by zero", rest["text"])
        self.assertNotIn("\x1b", rest["text"], "终端颜色码要去掉")
        self.assertIn("end of notebook; version ", rest["text"])
        # read_file 仍然返回 JSON，只多一条指向 read_notebook 的提示。
        raw = result_text(client.call_tool("read_file", {"path": "analysis.ipynb"}))
        self.assertIn('"cell_type": "markdown"', raw)
        self.assertIn("read_notebook shows its cells", raw)

    def test_edit_notebook_replaces_inserts_and_deletes_cells(self):
        self.add_notebook()
        self.write_config("coding")
        client = self.client("demo", "coding", "neutral-notebook-edit")
        # version 在最后一个文本块的页脚里，和 read_file 一样。
        footer = client.call_tool("read_notebook", {"path": "analysis.ipynb"})["content"][-1]["text"]
        version = footer.rsplit("; version ", 1)[1].rstrip("]")
        got = client.call_tool("apply_patch", {"files": [{
            "op": "edit_notebook", "path": "analysis.ipynb", "version": version,
            "cells": [
                {"cell_id": "d0f19b3c", "edit_mode": "delete"},
                {"cell_id": "c4e8f7aa", "new_source": "df.describe()"},
                {"cell_id": "5a1c0e2f", "edit_mode": "insert", "cell_type": "code", "new_source": "import numpy as np\n"},
            ],
        }]})
        self.assertFalse(is_error(got), got)
        self.assertTrue(result_text(got).startswith("edit_notebook analysis.ipynb (3 cell edits, "), result_text(got))
        nb = json.loads((self.root / "analysis.ipynb").read_text(encoding="utf-8"))
        ids = [cell.get("id") for cell in nb["cells"]]
        self.assertEqual(len(ids), 5)
        self.assertEqual(ids[0], "5a1c0e2f")
        self.assertNotIn("d0f19b3c", ids)
        self.assertEqual(nb["cells"][1]["source"], ["import numpy as np\n"])
        replaced = nb["cells"][ids.index("c4e8f7aa")]
        self.assertEqual((replaced["source"], replaced["outputs"], replaced["execution_count"]),
                         (["df.describe()"], [], None))
        # nbformat 的写法没被打乱：一个空格缩进、结尾换行。
        text = (self.root / "analysis.ipynb").read_text(encoding="utf-8")
        self.assertTrue(text.startswith('{\n "cells": [\n'))
        self.assertTrue(text.endswith("}\n"))

        stale = client.call_tool("apply_patch", {"files": [{
            "op": "edit_notebook", "path": "analysis.ipynb", "version": version,
            "cells": [{"cell_id": "c4e8f7aa", "new_source": "again"}],
        }]})
        self.assertTrue(result_text(stale).startswith("CCNM_E_STALE_EPOCH:"), result_text(stale))

    # -- 后台命令；取消和断开时停掉命令（P41） --

    def background(self, client: McpClient, line: str) -> str:
        got = client.call_tool("exec_command", {"shell": line, "run_in_background": True})
        text = result_text(got)
        self.assertFalse(is_error(got), text)
        self.assertIn("\nrunning in the background as output_ref r-", text)
        return text.split("output_ref ", 1)[1].split()[0]

    def wait_pid(self, name: str) -> int:
        path = self.root / name
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if path.exists() and path.read_text().strip():
                return int(path.read_text())
            time.sleep(0.02)
        self.fail(f"{name} never appeared")

    def assert_gone(self, pid: int, within: float = 5) -> None:
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                return
            time.sleep(0.05)
        os.kill(pid, 9)
        self.fail(f"进程 {pid} 还在")

    def test_a_background_command_is_read_while_it_runs_and_waited_for(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-background")
        started = time.monotonic()
        ref = self.background(client, "echo first; sleep 1; echo second")
        self.assertLess(time.monotonic() - started, 0.8)

        now = result_text(client.call_tool("read_output", {"output_ref": ref}))
        self.assertIn("\n[running for ", now)

        started = time.monotonic()
        done = result_text(client.call_tool("read_output", {"output_ref": ref, "wait_ms": 10000}))
        self.assertLess(time.monotonic() - started, 5, "wait_ms 没在命令结束时提前返回")
        self.assertTrue(done.startswith("first\nsecond\n[end of stdout at 13 bytes]\n[exited 0 after "), done)

    def test_stop_command_stops_what_run_in_background_started(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-stop")
        ref = self.background(client, "echo $$ > bg.pid; exec sleep 30")
        pid = self.wait_pid("bg.pid")
        got = client.call_tool("stop_command", {"output_ref": ref})
        self.assertFalse(is_error(got), result_text(got))
        self.assertIn("\nstopped by stop_command after ", result_text(got))
        self.assert_gone(pid)
        page = result_text(client.call_tool("read_output", {"output_ref": ref}))
        self.assertIn("\n[stopped by stop_command after ", page)

        foreground = result_text(client.call_tool("exec_command", {"shell": "true"}))
        fg_ref = foreground.rsplit("output_ref ", 1)[1].rstrip("]")
        refused = result_text(client.call_tool("stop_command", {"output_ref": fg_ref}))
        self.assertTrue(refused.startswith("CCNM_E_INVALID_ARGS:"), refused)

    def test_no_more_than_eight_run_in_the_background(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-eight")
        for n in range(8):
            self.background(client, f"echo $$ > bg{n}.pid; exec sleep 30")
        pids = [self.wait_pid(f"bg{n}.pid") for n in range(8)]
        ninth = client.call_tool("exec_command", {"shell": "sleep 30", "run_in_background": True})
        self.assertTrue(result_text(ninth).startswith("CCNM_E_INVALID_ARGS:"), result_text(ninth))
        self.assertIn("stop_command", result_text(ninth))

        # 会话结束时八个全停，server 马上退出。
        started = time.monotonic()
        client.close()
        self.assertLess(time.monotonic() - started, 8)
        for pid in pids:
            self.assert_gone(pid, within=1)

    def test_a_cancelled_call_stops_its_command(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-cancel")
        request = client.send("tools/call", {
            "name": "exec_command",
            "arguments": {"shell": "echo $$ > fg.pid; exec sleep 30", "timeout_ms": 60000},
        })
        pid = self.wait_pid("fg.pid")
        client.notify("notifications/cancelled", {"requestId": request, "reason": "user pressed esc"})
        self.assert_gone(pid)
        # 连接还在，照常回答。
        self.assertIn("workspace demo", result_text(client.call_tool("workspace_info", {})))

    def test_disconnecting_stops_running_commands_and_the_server_exits_at_once(self):
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-disconnect")
        self.background(client, "echo $$ > bg.pid; exec sleep 30")
        client.send("tools/call", {
            "name": "exec_command",
            "arguments": {"shell": "echo $$ > fg.pid; exec sleep 30", "timeout_ms": 60000},
        })
        pids = [self.wait_pid("bg.pid"), self.wait_pid("fg.pid")]
        started = time.monotonic()
        client.close()
        # 改之前：server 等前台命令自己跑完（这里是 30 秒）才退出，一直占着写锁。
        self.assertLess(time.monotonic() - started, 8)
        for pid in pids:
            self.assert_gone(pid, within=1)
        # 写锁已经放了：同一个 workspace 马上能开新的 coding 会话。
        again = self.client("demo", "coding", "neutral-disconnect-again")
        self.assertIn("workspace demo", result_text(again.call_tool("workspace_info", {})))

    # -- 生命周期契约：取消等待不等于取消命令，断了就是断了（P42） --

    def test_cancelling_a_wait_leaves_the_command_running(self):
        """取消一次等待只是取消这次等待。

        取消 `exec_command` 的调用会停掉命令（上一个测试），取消 `read_output`
        的等待不会——**这两件事只差一个工具名**，而模型和 hub 都会按"超时了就
        取消"去做。命令的终点只有三个：它自己结束、`stop_command`、连接结束。
        """
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-cancel-wait")
        ref = self.background(client, "echo $$ > bg.pid; sleep 3; echo done")
        pid = self.wait_pid("bg.pid")

        waiting = client.send("tools/call", {
            "name": "read_output",
            "arguments": {"output_ref": ref, "wait_ms": 30000},
        })
        time.sleep(0.2)
        client.notify("notifications/cancelled", {"requestId": waiting, "reason": "调用方等不及了"})
        time.sleep(0.5)
        os.kill(pid, 0)  # 还在跑；停了的话这里抛 ProcessLookupError

        done = result_text(client.call_tool("read_output", {"output_ref": ref, "wait_ms": 10000}))
        self.assertTrue(done.startswith("done\n"), done)
        self.assertIn("\n[exited 0 after ", done)

    def test_stopping_a_command_twice_says_the_same_thing(self):
        """停一个已经停了的命令不是错误，照实报它怎么结束的。

        调用方重试、或者两个地方同时收手，都会来第二次。第二次报错会让人以为
        命令还在。
        """
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-stop-twice")
        ref = self.background(client, "echo $$ > bg.pid; exec sleep 30")
        pid = self.wait_pid("bg.pid")

        first = result_text(client.call_tool("stop_command", {"output_ref": ref}))
        self.assertIn("\nstopped by stop_command after ", first)
        self.assert_gone(pid)

        again = client.call_tool("stop_command", {"output_ref": ref})
        self.assertFalse(is_error(again), result_text(again))
        self.assertIn("\nstopped by stop_command after ", result_text(again))

    def test_an_output_ref_does_not_survive_the_connection(self):
        """断了就是断了：同名会话重连，旧 `output_ref` 什么都不是。

        这一条挡住的是"重连之后接着看那个后台任务"——契约里没有这种操作，而
        只要它一次侥幸成功，调用方就会当成能用。
        """
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-ref-gone")
        ref = self.background(client, "echo $$ > bg.pid; exec sleep 30")
        pid = self.wait_pid("bg.pid")
        client.close()
        self.assert_gone(pid, within=2)

        again = self.client("demo", "coding", "neutral-ref-gone")
        for tool in ("read_output", "stop_command"):
            with self.subTest(tool=tool):
                said = result_text(again.call_tool(tool, {"output_ref": ref}))
                self.assertTrue(said.startswith("CCNM_E_INVALID_ARGS:"), said)
                self.assertIn(ref, said)

    # -- 服务端自己验输入（P44） --

    def test_a_tool_with_side_effects_refuses_a_field_it_does_not_declare(self):
        """有副作用的工具拒绝未知字段，连嵌套里的也拒。

        gld 那边会先拦一道，但**外部 CLI 可以绕过 hub 直连**，那条路上没人替
        Runtime 检查。以前这些字段被 serde 静默丢掉，命令照跑、补丁照落盘——
        调用方于是以为自己关掉了什么。
        """
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-unknown-write")

        # 编一个看起来像安全开关的字段。
        said = result_text(
            client.call_tool("exec_command", {"cmd": ["/bin/echo", "hi"], "sandbox": False})
        )
        self.assertIn("unknown field `sandbox`", said)
        # 合法字段名一并列出来，模型能自己改对。
        self.assertIn("run_in_background", said)

        # 嵌套里的也拒：files[0] 多一个 mode。
        said = result_text(client.call_tool("apply_patch", {
            "files": [{"op": "add", "path": "new.txt", "content": "x\n", "mode": "0777"}],
        }))
        self.assertIn("unknown field `mode`", said)
        self.assertFalse((self.root / "new.txt").exists(), "拒绝要发生在落盘之前")

    def test_a_read_only_tool_answers_but_says_what_it_ignored(self):
        """只读工具不因为一个多余字段就失败，但也不装作看见了它。"""
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-unknown-read")
        said = result_text(
            client.call_tool("read_file", {"path": "hello.txt", "follow_symlinks": True})
        )
        self.assertIn("1→one", said, "读该照常给答案")
        self.assertIn("[ignored, this tool has no such argument: follow_symlinks", said)

    def test_a_ceiling_a_command_depends_on_is_refused_not_clamped(self):
        """写/执行这边超界是拒，不是悄悄改小——读那边照旧钳，但说一声。"""
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-ceiling")

        said = result_text(
            client.call_tool("exec_command", {"cmd": ["/bin/echo", "hi"], "timeout_ms": 99_999_999})
        )
        self.assertTrue(said.startswith("CCNM_E_INVALID_ARGS:"), said)
        self.assertIn("run_in_background", said, "要告诉它该怎么办")

        ref = self.background(client, "sleep 0.2")
        page = result_text(client.call_tool("read_output", {"output_ref": ref, "wait_ms": 99_999_999}))
        self.assertIn("[waited up to 600000 ms, not the 99999999 ms asked for", page)

    def test_the_published_schema_says_which_tools_refuse_extra_fields(self):
        """声明和真实解析必须一致（评审 X06）：schema 上写的就是服务端执行的。"""
        self.write_config("coding", unconfined=True)
        client = self.client("demo", "coding", "neutral-schema")
        extra = {
            tool["name"]: tool["inputSchema"].get("additionalProperties")
            for tool in client.tools()
        }
        for name in ("exec_command", "apply_patch", "stop_command"):
            self.assertIs(extra[name], False, f"{name} 该声明它不收额外字段")
        for name in ("read_file", "list_files", "search_text", "read_output"):
            self.assertIs(extra[name], True, f"{name} 收额外字段（只是会说忽略了）")
        # 没有参数结构的那个不发这个键，不要凭空造一个。
        self.assertIsNone(extra["workspace_info"])

    # -- 拒绝 --

    def test_a_workspace_without_the_opt_in_never_hands_out_a_session(self):
        for workspace in ("private", "no-such-workspace"):
            with self.subTest(workspace=workspace):
                out = self.refused(workspace, "read")
                self.assertNotEqual(out.returncode, 0)
                self.assertEqual(out.stdout, "", "不能有半个握手")
                self.assertTrue(out.stderr.startswith("CCNM_E_POLICY:"), out.stderr)

    def test_asking_for_more_than_configured_is_refused(self):
        out = self.refused("demo", "coding")  # 配置是 read
        self.assertNotEqual(out.returncode, 0)
        self.assertEqual(out.stdout, "")
        self.assertIn("read mode", out.stderr)

    def test_an_unknown_protocol_number_stops_the_session(self):
        # 比这个 build 认识的都大：必须停，不能被当成别的形状试着解。
        body = json.dumps({"protocol": 99, "workspace": "demo", "session": "x"}).encode()
        payload = base64.urlsafe_b64encode(body).decode().rstrip("=")
        out = subprocess.run(
            [str(BINARY), "internal", "mcp-serve", "--payload", payload],
            env=self.env(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertTrue(out.stderr.startswith("CCNM_E_VERSION:"), out.stderr)

    # -- 流本身 --

    def test_stdout_carries_nothing_but_protocol(self):
        client = self.client("demo", "read", "neutral-stdout")
        client.tools()
        client.call_tool("read_file", {"path": "hello.txt"})
        self.assertTrue(client.lines, "总得说过点什么")
        for line in client.lines:
            message = json.loads(line)  # 不是 JSON 就直接抛
            self.assertEqual(message.get("jsonrpc"), "2.0", line)

    def test_the_handshake_does_not_describe_the_machine(self):
        client = self.client("demo", "read", "neutral-leak")
        instructions = client.initialize().get("instructions", "")
        # 说的是 workspace 和模式，不是这台机器在哪。
        self.assertIn("demo", instructions)
        self.assertNotIn(str(self.root), instructions)
        self.assertNotIn(str(self.dir / "home"), instructions)


if __name__ == "__main__":
    unittest.main()
