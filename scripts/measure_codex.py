#!/usr/bin/env python3
"""Capture Codex CLI evidence without changing config or copying credentials.

Usage: python3 scripts/measure_codex.py OUTPUT_DIR [inspect|seven-tools]
inspect never starts a model. seven-tools consumes the existing Codex login/quota
and operates only on disposable fixture files through the real ccnm MCP server.
"""

import argparse
import base64
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import tempfile

VERSION = "codex-cli 0.153.4"
TOOLS = [
    "workspace_info", "list_files", "search_text", "read_file",
    "apply_patch", "exec_command", "read_output",
]
DISABLED_FEATURES = [
    "shell_tool", "unified_exec", "view_image", "apps", "plugins", "hooks",
    "multi_agent", "multi_agent_v2", "browser_use", "computer_use",
    "image_generation", "memories", "workspace_dependencies", "skill_search",
    "shell_snapshot", "goals", "tool_suggest",
]
PROMPT = (
    "Controlled CCNM end-to-end fixture test. Use only the ccnm MCP tools, "
    "never native filesystem/shell/patch or other services. Call all seven "
    "tools in this exact order: (1) workspace_info, (2) list_files at the "
    "project root, (3) search_text for CCNM_RUNTIME_SENTINEL_7319, "
    "(4) read_file probe.txt and retain its exact version, (5) apply_patch "
    "using that version to change only probe.txt to CCNM_RUNTIME_PATCHED_7319 "
    "followed by newline, (6) exec_command with cmd [\"/bin/cat\",\"probe.txt\"], "
    "(7) read_output using the output_ref from that command. Report the "
    "resulting text and whether all seven calls succeeded. Do not spawn "
    "agents or do anything else. Stop."
)


def run(argv, *, cwd=None, env=None, stdin=b"", timeout=20):
    # A timeout must also stop the MCP child before its fixture is removed.
    process = subprocess.Popen(
        argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(stdin, timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate()
    return {
        "exit_code": process.returncode, "timed_out": timed_out,
        "stdout": stdout.decode("utf-8", errors="replace"),
        "stderr": stderr.decode("utf-8", errors="replace"),
    }


def redact(text, fixture=None):
    if fixture:
        # MCP canonicalizes /var to /private/var on macOS.
        for path in sorted({str(fixture), str(fixture.resolve())}, key=len, reverse=True):
            text = text.replace(path, "/fixture")
    text = text.replace(str(Path.home()), "<user-home>")
    text = re.sub(r"(?im)(Logged in using an API key)[^\n]*", r"\1 - <redacted>", text)
    text = re.sub(r"[\w.+-]+@[\w.-]+", "<redacted-email>", text)
    text = re.sub(r"\b[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\b",
                  "00000000-0000-4000-8000-000000000001", text)
    text = re.sub(r"req_[0-9a-f]+", "req_redacted", text)
    text = re.sub(r"cf-ray: [A-Za-z0-9-]+", "cf-ray: <redacted>", text)
    return text


def sanitize(value, fixture=None):
    if isinstance(value, str):
        return redact(value, fixture)
    if isinstance(value, dict):
        return {key: sanitize(item, fixture) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize(item, fixture) for item in value]
    return value


def save(directory, name, record, fixture=None):
    # Sanitize string values before encoding, so redaction cannot break JSON.
    text = json.dumps(sanitize(record, fixture), indent=2, ensure_ascii=False)
    (directory / name).write_text(text + "\n")


def inspect(codex, directory):
    for name, args in [
        ("version", ["--version"]), ("help", ["--help"]),
        ("exec-help", ["exec", "--help"]),
        ("auth-help", ["login", "status", "--help"]),
        ("auth-current", ["login", "status"]),
        ("features", ["features", "list"]),
    ]:
        save(directory, name + ".json", run([codex, *args]))
    with tempfile.TemporaryDirectory(prefix=f"ccnm-codex-auth-{os.getpid()}-") as temp:
        env = os.environ.copy()
        env["CODEX_HOME"] = temp
        save(directory, "auth-empty-home.json", run(
            [codex, "login", "status"], cwd=temp, env=env,
        ), Path(temp))


def seven_tools(codex, directory, ccnm):
    with tempfile.TemporaryDirectory(prefix=f"ccnm-codex-mcp-{os.getpid()}-") as temp:
        fixture = Path(temp)
        agent, runtime, home = [fixture / name for name in ("agent", "runtime", "runtime-home")]
        for path in (agent, runtime, home):
            path.mkdir()
        (runtime / "probe.txt").write_text("CCNM_RUNTIME_SENTINEL_7319\n")
        (agent / "probe.txt").write_text("WRONG_AGENT_NODE_9520\n")
        config = home / "config/ccnm"
        config.mkdir(parents=True)
        # This is an explicit scratch-only opt-in, not a production safety audit.
        (config / "config.toml").write_text(
            'version=1\nthis="runtime"\n[nodes.runtime]\n[nodes.agent]\n'
            'ssh="unused-fixture"\n[workspaces.fixture]\nagent_node="agent"\n'
            f'root={json.dumps(str(runtime))}\nallow_unconfined_exec=true\n'
        )
        payload = base64.urlsafe_b64encode(json.dumps({
            "protocol": 1, "workspace": "fixture", "root": str(runtime),
            "session": "codex-probe", "policy": "coding", "interactive": False,
        }, separators=(",", ":")).encode()).decode().rstrip("=")
        transport = [
            "-i", "PATH=/usr/bin:/bin", "USER=fixture", "HOME=" + str(home),
            "XDG_CONFIG_HOME=" + str(home / "config"),
            "XDG_STATE_HOME=" + str(home / "state"), str(ccnm),
            "internal", "mcp-serve", "--payload", payload,
        ]
        argv = [
            codex, "exec", "--ignore-user-config", "--ignore-rules",
            "--skip-git-repo-check", "--ephemeral", "--json", "--color", "never",
            "--sandbox", "read-only", "-c", 'approval_policy="never"',
            "-c", 'web_search="disabled"',
        ]
        for feature in DISABLED_FEATURES:
            argv += ["--disable", feature]
        argv += [
            "--enable", "code_mode_only", "-c", "agents.enabled=false",
            "-c", 'features.code_mode.excluded_tool_namespaces=["functions","collaboration"]',
            "-c", 'mcp_servers.ccnm.command="/usr/bin/env"',
            "-c", "mcp_servers.ccnm.args=" + json.dumps(transport),
            "-c", "mcp_servers.ccnm.required=true",
            "-c", "mcp_servers.ccnm.enabled_tools=" + json.dumps(TOOLS),
            "-c", 'mcp_servers.ccnm.default_tools_approval_mode="approve"', "-",
        ]
        result = run(argv, cwd=agent, stdin=PROMPT.encode(), timeout=120)
        for stream in ("stdout", "stderr"):
            (directory / f"seven-tools.{stream}").write_text(redact(result[stream], fixture))
        # Record arguments without keeping an opaque payload hiding a local path.
        recorded_argv = [arg.replace(payload, "<fixture-payload-generated-above>") for arg in argv]
        events = [json.loads(line) for line in result["stdout"].splitlines() if line.strip()]
        completed = [event["item"] for event in events if event.get("type") == "item.completed"
                     and event.get("item", {}).get("type") == "mcp_tool_call"]
        outcome = {
            "version": VERSION, "argv": recorded_argv, "stdin": PROMPT,
            "exit_code": result["exit_code"], "timed_out": result["timed_out"],
            "terminal_event": events[-1].get("type") if events else None,
            "completed_tools": [item["tool"] for item in completed],
            "all_tools_succeeded": all(item["status"] == "completed" for item in completed),
            "runtime_file": (runtime / "probe.txt").read_text(),
            "agent_file": (agent / "probe.txt").read_text(),
        }
        save(directory, "seven-tools.json", outcome, fixture)
        return (
            result["exit_code"] == 0 and not result["timed_out"]
            and outcome["terminal_event"] == "turn.completed"
            and outcome["completed_tools"] == TOOLS and outcome["all_tools_succeeded"]
            and outcome["runtime_file"] == "CCNM_RUNTIME_PATCHED_7319\n"
            and outcome["agent_file"] == "WRONG_AGENT_NODE_9520\n"
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new output directory; existing paths are refused")
    parser.add_argument("mode", nargs="?", choices=("inspect", "seven-tools"), default="inspect")
    args = parser.parse_args()
    codex = shutil.which("codex")
    if not codex:
        parser.error("codex not found on PATH")
    version = run([codex, "--version"])
    if version["exit_code"] != 0 or version["stdout"].strip() != VERSION:
        parser.error(f"this measured probe requires {VERSION}; inspect a different version first")
    ccnm = Path(__file__).resolve().parent.parent / "target/debug/ccnm"
    if args.mode == "seven-tools" and not os.access(ccnm, os.X_OK):
        parser.error("build the local binary first: cargo build -p ccnm-cli")
    try:
        args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
    except FileExistsError:
        parser.error(f"refusing to overwrite existing output: {args.output}")
    inspect(codex, args.output)
    if args.mode == "seven-tools":
        if not seven_tools(codex, args.output, ccnm):
            raise SystemExit("measurement failed; preserved output must be inspected, not blessed")
    print(f"Captured {args.mode} evidence in {args.output}")


if __name__ == "__main__":
    main()
