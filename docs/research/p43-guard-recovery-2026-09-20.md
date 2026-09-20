# P43 停不掉的命令不交出写权（2026-09-20）

环境：macOS 26.6.2 arm64，rustc 1.98.0。零模型额度，没上真机。故障注入用真实 `ccnm internal mcp-serve` 加一个不 import 任何 ccnm 代码的中立 MCP 客户端。

来源是主评审 `toexec/docs/plan/2026-09-19-cross-project-refactor-review.md` 的 X05，和本仓[落地清单](2026-09-19-cross-project-refactor-actions.md)的 C-B。

## 1. 结论

- **查出一条真缺口**：一个信号够不着的后代留下时，server 照常正常退出、把写锁标成 `released`，下一个 coding 会话就在同一棵树上开起来了。X05 验收第一句就是不许这样。已修：停不掉就不交权。
- `SIGKILL` 那条锁的表现本来就是对的（保守拒绝），只是它起的命令留在机器上——那是 P41 记过的缺口，本轮没动。
- **同一棵树配两个 `XDG_STATE_HOME` 就是两个互不知晓的写域**，两个 coding 会话能同时写。这是设计边界，但此前一个字的文档都没有；本轮写进了运维手册和协议。
- pid 进了 marker，只为让诊断说得准。**pid 不在从来不是交权的理由。**

## 2. 故障注入：三种，各自现在怎样

| 注入什么 | 怎么造 | 结果 |
| --- | --- | --- |
| `mcp-serve` 被 `SIGKILL` | 起一个后台 `sleep 120`，对 server 发 `SIGKILL` | 它起的命令留在机器上；marker 停在 `held`（没机会写 `released`）；下一个会话报 `CCNM_E_POLICY` 被拒。**锁这一半是对的** |
| 一个信号够不着的后代 | 命令里 `fork` 出一个进程，`setsid` 离开进程组，**又留着 stdout 那根管道**；父进程立刻退出 | `stop_all` 两段各等 10 秒后放弃，server **退出码 0**，marker 被写成 `released`，**下一个 coding 会话拿到了写权**——旧的那个还在那棵树上 |
| 同一棵树，两个 `XDG_STATE_HOME` | 同一份 config、同一个 root，两个 server 各给一个 state 目录 | 两个 coding 会话同时开着，**各自都成功写了文件**；两边的 marker 文件名相同（resource 路径一样）但在各自的 state 目录下，谁也不知道谁 |

第二条是本轮要修的。造它的那一行（探针里用的）：

```sh
python3 -c "import os,sys,time;pid=os.fork();open('escaped.pid','w').write(str(pid)) if pid else None;sys.exit(0) if pid else None;os.setsid();time.sleep(120)"
```

那 20.1 秒是 `jobs::STOP_GIVE_UP` 的两段：先并行停每条命令（够不着的等满 10 秒放弃），再等每个 waiter 写完结果（同样 10 秒）。

## 3. 改了什么

**停不掉就不交权。**`Jobs::stop_all` 从返回 `bool` 改成返回**它放弃了哪几条**（有 `output_ref` 的报 ref，还没拿到的报 `run #<id>`）。server 退出前只要这个列表非空，就调 `WriteGuard::abandon`：marker 不写 `released`，而是留成

```text
held <session> <workspace> pid <pid>
abandoned 1 command(s) (r-e69acf4e804643a2)
```

下一个会话因此被拒，**话和崩溃那种不一样**——这不是异常退出，是 ccnm 知道剩了什么而故意不交权，所以它点名还剩哪个 ref、去哪找那条命令的命令行：

```text
CCNM_E_POLICY:
workspace write guard was kept on purpose: the session that held it ended with 1 command(s)
(r-e69acf4e804643a2) it could not stop, and those can still write this working tree, so
authority is not transferred
the session (cb-escape) ran as pid 25669, which is gone.
**That does not clear this**: commands it started can outlive it, and nothing here can see them
recover on the Runtime Node, in this order:
1. end what is named above. Each output_ref's command line is in
   ${XDG_STATE_HOME:-~/.local/state}/ccnm/sessions/cb-escape/output/<ref>/status;
   look for the process group it left behind
2. only then back up and delete the single marker naming cb-escape in
   ${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/
never clear it just because time passed
```

**marker 里记 pid。**拒绝时查一次那个 pid 现在是什么，据此分三种说法：还在跑（把命令行一起给出来，"先把它停掉"）、已经不在、被别的程序复用了。后两种仍然是**拒绝**——评审 X05 写得很清楚：不能仅凭 PID、进程不存在一次或等某个固定时长就清锁。pid 只是省掉"在进程表里翻 mcp-serve"那一步。

旧格式（没有 pid 的 marker）照旧能读、照旧拒绝，诊断里说明"这个 marker 早于 pid 记录"。

**`ccnm status` 跟着分情况。**它原来一律说"异常退出留下的"。现在 marker 带 `abandoned` 时说的是"故意留着的，有什么没停掉，先收掉它们"——按老话去删 marker，正好会把第二个写者放进同一棵树。

## 4. 这一轮没有做的

- **进程容器**（supervisor、cgroup、job object）。X05 说"可采用"，那是另一个范围。没有它，"一个脱离进程组又**不**占管道的后代"这种情况 ccnm 根本发现不了：`stop_all` 会正常返回，锁照常交出去。**这条边界仍然存在**，写在支持矩阵里。
- **跨 state home 的共享锁服务**。落地清单明说近期不要造，只把边界写清楚。
- 主机重启后的自动恢复。重启会让 flock 消失而 marker 还在，现在照旧是保守拒绝、要人来看。理论上"boot id 变了"能证明旧进程一定不在，但仍然证明不了它没在重启前改坏什么，而且这条要跨平台拿 boot 标识，留给以后。
- 真实 Host、真实模型、Linux：都没跑。
