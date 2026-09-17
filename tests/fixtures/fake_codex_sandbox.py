#!/usr/bin/env python3
"""A stand-in for `codex` where only two verbs matter, for tests that cannot
assume Codex is installed (CI has no Codex):

    codex --version                                   prints FAKE_CODEX_VERSION
    codex sandbox --sandbox-state-json J -- ARGV...   records J and ARGV, then runs ARGV

It does no sandboxing at all. What it proves is what ccnm sent it: the
state JSON, the argv, the cwd, the CODEX_HOME -- one JSON line per call
appended to FAKE_SANDBOX_LOG -- and that the command really ran behind the
wrapper (it sees FAKE_SANDBOXED=1). With FAKE_SANDBOX_FAIL set, every
`sandbox` call prints that line to stderr and exits 1 without running
anything: a machine where the sandbox cannot be set up. The real thing is exercised by the
test that runs only with CCNM_TEST_CODEX_BIN.
"""
import json
import os
import sys


def main():
    argv = sys.argv[1:]
    if argv == ["--version"]:
        print(os.environ.get("FAKE_CODEX_VERSION", "codex-cli 0.154.0"))
        return 0
    if len(argv) >= 4 and argv[0] == "sandbox" and argv[1] == "--sandbox-state-json" and argv[3] == "--":
        state, command = argv[2], argv[4:]
        if os.environ.get("FAKE_SANDBOX_FAIL"):
            # The sandbox cannot be set up on this machine: what bubblewrap
            # says, and exit 1, as measured.
            print(os.environ["FAKE_SANDBOX_FAIL"], file=sys.stderr)
            return 1
        log = os.environ.get("FAKE_SANDBOX_LOG")
        if log:
            with open(log, "a") as f:
                f.write(json.dumps({"state": json.loads(state), "argv": command, "cwd": os.getcwd(),
                                    "codex_home": os.environ.get("CODEX_HOME")}) + "\n")
        env = dict(os.environ)
        env["FAKE_SANDBOXED"] = "1"
        try:
            os.execvpe(command[0], command, env)
        except OSError as e:
            # What sandbox-exec prints on macOS, and its exit code.
            print(f"sandbox-exec: execvp() of '{command[0]}' failed: {e.strerror}", file=sys.stderr)
            return 71
    print(f"unexpected arguments: {argv}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
