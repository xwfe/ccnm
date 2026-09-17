#!/usr/bin/env python3
"""A stand-in for `codex exec-server --listen stdio`, for tests that cannot
assume Codex is installed (CI has no Codex).

It speaks the 0.154.0 wire shape closely enough for ccnm's supervisor and does
real work: files are really written, processes really started. It ignores the
sandbox entirely -- which is exactly what makes it useful, because whatever
reaches it happens, so a test can tell "refused by ccnm" from "refused by a
sandbox" by looking at the disk.

Behaviour copied from the real executor (toexec G01, ccnm P22):
- each process gets its own session, so killing this process's group does not
  reach it;
- on stdin EOF it kills the processes it started and exits 0;
- an unknown notification closes the connection.

Knobs, all environment variables:
  FAKE_EXEC_LOG          append every message received, one JSON per line
  FAKE_CODEX_VERSION     what `--version` prints (default codex-cli 0.154.0)
  FAKE_EXEC_CRASH_ON     a method name; receiving it exits 7 at once
  FAKE_EXEC_CHATTER      COUNT:SIZE; after `initialized`, another thread writes
                         COUNT `process/output` notifications of SIZE chunk
                         characters while requests are being answered, the
                         way a command's output interleaves with replies
"""

import base64
import json
import os
import signal
import subprocess
import sys
import threading
from urllib.parse import unquote, urlparse


class NotFound(Exception):
    pass


def path_of(uri):
    return unquote(urlparse(uri).path)


STDOUT = threading.Lock()


def send(message):
    line = json.dumps(message) + "\n"
    with STDOUT:
        sys.stdout.write(line)
        sys.stdout.flush()


def chatter(count, size):
    for seq in range(count):
        send({"method": "process/output", "params": {
            "processId": "chatter", "seq": seq, "stream": "stdout", "chunk": "A" * size}})


def handle(method, params, children):
    if method == "initialize":
        return {"sessionId": "fake", "environmentInfo": {
            "executorVersion": "0.0.0", "providerId": "sha256:fake"}}
    if method == "fs/writeFile":
        with open(path_of(params["path"]), "wb") as f:
            f.write(base64.b64decode(params["dataBase64"]))
        return {}
    if method == "fs/readFile":
        try:
            with open(path_of(params["path"]), "rb") as f:
                return {"dataBase64": base64.b64encode(f.read()).decode()}
        except FileNotFoundError as e:
            raise NotFound from e
    if method == "fs/getMetadata":
        path = path_of(params["path"])
        if not os.path.lexists(path):
            raise NotFound
        return {"isDirectory": os.path.isdir(path), "isFile": os.path.isfile(path),
                "isSymlink": os.path.islink(path), "size": 0}
    if method == "process/start":
        env = dict(os.environ)
        env.update(params.get("env") or {})
        proc = subprocess.Popen(params["argv"], cwd=path_of(params["cwd"]), env=env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, start_new_session=True)
        children.append(proc)
        return {"processId": params["processId"]}
    return {}


def main():
    if sys.argv[1:] == ["--version"]:
        print(os.environ.get("FAKE_CODEX_VERSION", "codex-cli 0.154.0"))
        return 0
    if sys.argv[1:] != ["exec-server", "--listen", "stdio"]:
        print(f"unexpected arguments: {sys.argv[1:]}", file=sys.stderr)
        return 2
    log = os.environ.get("FAKE_EXEC_LOG")
    crash_on = os.environ.get("FAKE_EXEC_CRASH_ON")
    children = []
    for line in sys.stdin:
        message = json.loads(line)
        if log:
            with open(log, "a") as f:
                f.write(json.dumps(message) + "\n")
        method, mid = message.get("method"), message.get("id")
        if method == crash_on:
            os._exit(7)
        if mid is None:
            if method == "initialized":
                if os.environ.get("FAKE_EXEC_CHATTER"):
                    count, size = map(int, os.environ["FAKE_EXEC_CHATTER"].split(":"))
                    threading.Thread(target=chatter, args=(count, size), daemon=True).start()
                continue
            break
        try:
            send({"id": mid, "result": handle(method, message.get("params") or {}, children)})
        except NotFound:
            send({"id": mid, "error": {"code": -32004,
                                       "message": "No such file or directory (os error 2)"}})
    for proc in children:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
