"""P106.2：一条 ssh 连接上跑一小时、来回送约 100 MB 随机字节，逐字节比对。

    P106_SSH='ssh -T -o … user@host' python3 -I p106-long-run.py 'C:/…/p106.exe echo-mt awake' 日志文件

远端用 p106-windows-probe.cs 的 echo-mt（读写各一个线程，和 ccnm mcp-serve 的结构一样），
awake 让它运行期间请求系统别睡。节奏：随机 1–64 KiB 一块，平均约 33 KB/s；第 20、40 分钟
各停 5 分钟，什么都不送（看空闲时连接还在不在、保活起不起作用）。每分钟写一行进度。
结束时关 stdin，等远端回完，比对总字节数和 sha256。
"""
import hashlib
import os
import random
import shlex
import subprocess
import sys
import threading
import time

remote, log_path = sys.argv[1], sys.argv[2]
TOTAL_SECS = 3600
IDLE = [(20 * 60, 25 * 60), (40 * 60, 45 * 60)]
RATE = 33_000  # 字节/秒

log = open(log_path, "a", buffering=1)


def say(msg):
    log.write(time.strftime("%H:%M:%S ") + msg + "\n")


p = subprocess.Popen(shlex.split(os.environ["P106_SSH"]) + [remote],
                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
sent_hash, recv_hash = hashlib.sha256(), hashlib.sha256()
sent = bytearray()  # 留着发出去的全部字节，回来的逐段比对
state = {"sent": 0, "recv": 0, "mismatch_at": None, "send_error": None}
lock = threading.Lock()


def reader():
    while True:
        chunk = p.stdout.read1(65536)
        if not chunk:
            return
        with lock:
            start = state["recv"]
            if state["mismatch_at"] is None and bytes(sent[start:start + len(chunk)]) != chunk:
                state["mismatch_at"] = start
            state["recv"] += len(chunk)
            recv_hash.update(chunk)


def writer():
    t0 = time.monotonic()
    rng = random.Random(106)
    try:
        while True:
            el = time.monotonic() - t0
            if el >= TOTAL_SECS:
                break
            if any(a <= el < b for a, b in IDLE):
                time.sleep(1)
                continue
            n = rng.randint(1024, 65536)
            data = os.urandom(n)
            with lock:
                sent.extend(data)
                state["sent"] += n
            sent_hash.update(data)
            p.stdin.write(data)
            p.stdin.flush()
            time.sleep(n / RATE)
        p.stdin.close()
    except (BrokenPipeError, OSError) as e:
        state["send_error"] = repr(e)


rt = threading.Thread(target=reader, daemon=True)
wt = threading.Thread(target=writer, daemon=True)
say(f"start remote={remote!r}")
rt.start()
wt.start()
t0 = time.monotonic()
while wt.is_alive() or rt.is_alive():
    time.sleep(60)
    with lock:
        say(f"t={int(time.monotonic() - t0)}s sent={state['sent']} recv={state['recv']} "
            f"lag={state['sent'] - state['recv']} mismatch_at={state['mismatch_at']} send_error={state['send_error']}")
    if not wt.is_alive() and rt.is_alive() and time.monotonic() - t0 > TOTAL_SECS + 300:
        say("reader still waiting 5 min after stdin closed; killing ssh")
        p.kill()
        break
p.wait()
err = p.stderr.read().replace(b"\r", b"")
err = "\n".join(l for l in err.decode(errors="replace").split("\n") if l and not l.startswith("**"))
ok = state["sent"] == state["recv"] and sent_hash.digest() == recv_hash.digest() and state["mismatch_at"] is None
say(f"END rc={p.returncode} sent={state['sent']} recv={state['recv']} identical={ok} "
    f"mismatch_at={state['mismatch_at']} send_error={state['send_error']} stderr={err!r}")
print("identical" if ok else "FAILED")
