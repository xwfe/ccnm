#!/usr/bin/env python3
"""Measurement-only stdio MCP transport; this does not enable a ccnm provider.

All paths in the arguments belong to the Runtime Node. Agent CODEX_HOME is
never an argument, payload field or remote environment value.
"""
import argparse
import base64
import json
import os
from pathlib import PurePosixPath
import re
import shlex

SENSITIVE_PREFIXES = ("CODEX_", "OPENAI_", "CLAUDE_", "ANTHROPIC_")


def ssh_environment(source):
    """Keep local SSH authentication usable, without forwarding Agent state."""
    return {key: value for key, value in source.items()
            if not key.startswith(SENSITIVE_PREFIXES)}


def token(value, label):
    if not re.fullmatch(r"[A-Za-z0-9._-]+", value) or value.startswith("-") or value in (".", ".."):
        raise ValueError(f"{label} must be a plain token, not an option or path")
    return value


def absolute(value):
    path = PurePosixPath(value)
    if not path.is_absolute() or ".." in path.parts or any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError("Runtime paths must be absolute, without '..' or control characters")
    return value


def command(alias, binary, root, config, home, session):
    token(alias, "SSH alias")
    token(session, "session")
    for value in (binary, root, config, home):
        absolute(value)
    payload = {
        "protocol": 1, "workspace": "fixture", "root": root,
        "session": session, "policy": "coding", "interactive": True,
    }
    wire = base64.urlsafe_b64encode(json.dumps(payload, separators=(",", ":")).encode()).decode().rstrip("=")
    remote = [
        "/usr/bin/env", "-i", "PATH=/opt/homebrew/bin:/usr/bin:/bin",
        "USER=ccnm-probe", "HOME=" + home,
        "XDG_CONFIG_HOME=" + home + "/config", "XDG_STATE_HOME=" + home + "/state",
        "CCNM_CONFIG=" + config, binary, "internal", "mcp-serve", "--payload", wire,
    ]
    return [
        "/usr/bin/ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10",
        "-o", "ClearAllForwardings=yes", "-o", "ForwardAgent=no",
        "-o", "ControlMaster=no", "-o", "ControlPath=none",
        "-o", "SendEnv=-*", "-T", alias, shlex.join(remote),
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("alias", "binary", "root", "config", "home", "session"):
        parser.add_argument(name)
    args = parser.parse_args()
    try:
        argv = command(args.alias, args.binary, args.root, args.config, args.home, args.session)
    except ValueError as error:
        parser.error(str(error))
    # No shell or captured output here: stdin/stdout remain exactly MCP stdio.
    os.execvpe(argv[0], argv, ssh_environment(os.environ))


if __name__ == "__main__":
    main()
