"""P106：经 ssh 跑一条 Windows 上的远端命令，喂给定字节，限时，报回来多少。

配合 p106-windows-probe.cs（在 Windows 上用 .NET Framework 自带的 csc.exe 编译）用：

    P106_SSH='ssh -T -o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 ... user@host' \
      python3 -I p106-stdin-driver.py 300000 40 'C:/…/p106.exe echo'

P106_SSH 写 ccnm 传输连接用的 ssh 参数（crates/ccnm-core/src/ssh.rs 的
command() 加 transport_options()）。模式：
- 默认（echo）：回来的字节和输入比对；
- P106_SINK=1（配 `p106.exe sink`）：远端读到 EOF 后打印字节数和 sha256，这里打印本地 sha256 对照；
- P106_SOURCE=1（配 `p106.exe source N`）：只收，打印收到的字节数和 sha256；
- P106_PACE=块大小,毫秒：匀速写、写完关 stdin（不设就一次写完）。
超时时杀掉本地 ssh；远端进程不会跟着退出（2026-10-24 实测），要另外收。
"""
import hashlib
import os
import shlex
import subprocess
import sys
import threading
import time

size, limit = int(sys.argv[1]), float(sys.argv[2])
argv = shlex.split(os.environ["P106_SSH"]) + [" ".join(sys.argv[3:])]
data = os.urandom(size)
pace = os.environ.get("P106_PACE")
t0 = time.monotonic()
p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
killed = []
timer = threading.Timer(limit, lambda: (killed.append(1), p.kill()))
timer.start()
if pace:
    chunk, ms = (int(x) for x in pace.split(","))

    def feed():
        try:
            for k in range(0, len(data), chunk):
                p.stdin.write(data[k:k + chunk])
                p.stdin.flush()
                time.sleep(ms / 1000)
            p.stdin.close()
        except BrokenPipeError:
            pass

    threading.Thread(target=feed, daemon=True).start()
    out = p.stdout.read()
    p.wait()
    err = p.stderr.read()
else:
    out, err = p.communicate(data)
timer.cancel()
rc = "timeout" if killed else p.returncode
dt = time.monotonic() - t0
# 本机新版 ssh 连 9.5 时 stderr 开头那三行后量子警告以 ** 开头，去掉再看远端说了什么
err = b"\n".join(l for l in err.replace(b"\r", b"").split(b"\n") if l and not l.startswith(b"**"))
err = err.decode(errors="replace")
if os.environ.get("P106_SINK"):
    print(f"in={size} local_sha={hashlib.sha256(data).hexdigest()[:16]} rc={rc} secs={dt:.1f} "
          f"remote={out.decode(errors='replace').strip()[:120]!r} stderr={err!r}")
elif os.environ.get("P106_SOURCE"):
    print(f"out={len(out)} local_sha={hashlib.sha256(out).hexdigest()[:16]} rc={rc} secs={dt:.1f} stderr={err!r}")
else:
    print(f"in={size} out={len(out)} identical={out == data} prefix_ok={data[:len(out)] == out} "
          f"rc={rc} secs={dt:.1f} stderr={err!r}")
