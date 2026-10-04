# P68 监督进程丢了不再等满超时、原始输出丢了不再当成空（2026-10-04）

接 [P62 续跑记录](2026-10-04-p62-resume-release.md)第 6.1、6.2 节查出的 F22、F23，它们是 P62.4 剩下的阻塞。**修复只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0（clippy 另用 1.99.0 跑一遍，CI 的 stable 就是它）；没有跑模型、没有推送。**同日晚在 P62 的真机拓扑上复验通过**，见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)。第 6 节是验收之外的事：按用户要求把日用的本机和 fodelf 换成了这个构建。`ccnm.machine/1` 的线格式、内部协议号都没变。两处修的都是 Agent 那一端，要 Agent Node 装的是新构建才生效。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P68.1 F22 | `agent-run` 每 2 秒用 `ps` 核一次监督进程，没了就以 `CCNM_E_INTERNAL` 收尾，Operator 读成 `unknown`。回归在旧代码上等满 31 秒才报错，修后整个测试文件 2.5 秒 | `5e0c580` |
| P68.2 F22 的标签 | 核对后**不改**：`agent_refused` 就是这种情况该有的值。协议第 9 节的举例补上了"没有结局""原始输出不在了"两种 | 本记录同一提交 |
| P68.3 F23 | 跑过的会话缺原始输出，`agent-output` 拒绝建视图，Operator 退回旧尾部加 `agent_refused`。回归在旧代码上回 `view_bytes 0、source_bytes 0、source_truncated false`，与真机一致 | `f5df624` |
| P68.4 门禁与文档 | 见第 5 节 | 本记录同一提交 |

## 2. F22：监督进程没了，几秒内收尾

**背景**：打印模式下，Agent 上的 `ccnm internal agent-run`（经 ssh 被 Operator 调起）请 Controller 起一个监督进程 `ccnm internal supervise`，自己等监督进程最后写下的结局文件 `exit`。监督进程是 Claude/Codex 的父进程，用管道把它们的输出转存到会话目录。

**缺陷**：监督进程被 SIGKILL 后永远不会写 `exit`，`agent-run` 却只认这个文件，要等到"会话超时 + 30 秒"才放弃。真机上默认超时 900 秒，调用方看了 15 分半的 `running`（发了 stop 之后是 `stopping`）。

**改法**（`session::wait_for_outcome` 加一个参数，`work::supervisor_gone` 回答它）：

1. 等结局时每 2 秒（`SUPERVISOR_CHECK`）问一次 `ps -ww -p <pid> -o stat= -o command=`，pid 是 `agent-run` 自己写下的那个。
2. 算"没了"的三种：没有这个进程；只剩僵尸（`stat` 以 `Z` 开头）；这个 pid 上跑的不是**这个会话的** `internal supervise`——按命令行最后四个词 `internal supervise --payload <wire>` 认，再解开 wire 比会话目录，和精确 stop 发信号前核对监督进程用的是同一种认法。第三种就是 pid 被别的进程复用了，那监督进程本身也已经不在。
3. `ps` 本身失败、回答不了，**不算没了**，照旧等到原来的期限。
4. 判定没了之后先再读一次 `exit`：监督进程是先写结局再退出的，两件事正好夹在"读文件"和"问 ps"之间的话，结局已经在盘上，按结局回。
5. 仍然没有，就返回 `CCNM_E_INTERNAL`：`the supervisor is gone and left no exit record at …; how the Agent ended is unknown, and the Agent it started (pid N) may still be running -- see …/supervisor.log`。

Operator 那边不用改：`after_dispatch` 本来就把派发之后的 `Internal` 读成 `unknown`，`failure` 带 `-32603` 和这段话（家目录前缀换成 `~`）。

**有意不做的**：

- **不替监督进程写结局。** 写了，Agent 上的 `ccnm log` 和 `session_state` 就会说它"结束了"；可被 launchd 收养的 Claude/Codex 可能还在跑、还连着 Runtime。没人看见它结束，`unknown` 才是实话。Agent 上 `session_state` 对"记了 pid、进程不在、没有结局"的会话本来就回 `Unknown`。
- **不杀被收养的 Agent 进程。** 它已经不归任何 ccnm 进程管，按 pid 去杀就是"凭一个可能被复用的 pid 发信号"，这是这个项目一直拒绝的做法（[core-hardening](../plan/core-hardening.md) 第 7 节"强杀/脱组后代自动回收"）。`failure` 里给出它的 pid，交给人去看。
- **为什么是 2 秒**：一次 `ps` 在本机约十几毫秒，一次 900 秒的运行最多多出 450 次；调用方本来就是轮询 `session.status`，晚 2 秒知道没有差别。原来的期限（超时 + 30 秒）原样保留，给"问不出来"的情况兜底。

**回归**：`a_supervisor_that_dies_without_an_outcome_is_noticed_in_seconds`（`crates/ccnm-cli/tests/instance_execution.rs`）。真实的 `ccnm internal agent-run`，经假 `ssh` 连到真实二进制扮的 Runtime（预检、写锁都是真的），Controller 用真的 `Listener` 起一个替身监督进程：它像真的一样先建好 `stdout`/`stderr`，然后 `kill -KILL $$`。会话超时设 1 秒。

| 构建 | 结果 |
| --- | --- |
| 修之前（`8636b09`） | 31.66 秒后失败：`no exit record after 31s … the supervisor did not finish` |
| 修之后 | 通过；整个测试文件 10 条 2.5 秒 |

为了这条用例，把原来那条"真实 agent-run + 假 Controller + 假监督进程"的搭建抽成了 `bound_print_run`，原用例的断言一条没动。另有单元测试：`ps` 回答的几种行（自己的、路径里带空格的、僵尸、别的会话的、别的程序、解不开的 wire）和 `ps` 失败的情况（`work::tests`）；"没了"在下一次检查时结束等待且不写结局、"写完结局才走"的竞态按结局回、"问不出来"等到期限（`session::tests`）。

## 3. F22 的标签：核对后不改

真机那次 `output.unavailable_reason` 是 `agent_refused`，续跑记录当时判断"标签不对，Agent 实际说的是还没结束"。P68.2 核对的结论是**这个判断不成立**：

- 协议第 9 节里 `unavailable_reason` 是封闭的三个值（schema 写成 `enum`）。第 13 节允许的加法里没有"给枚举加新值"，按 schema 校验的客户端会把没见过的值当错误。所以能选的只有这三个。
- `agent_unreachable` 的意思是"这次联系不上，稍后再取可能拿得到"。这次 Agent 联系得上；而且监督进程是 Agent 输出的转存者，它死了之后 Agent 再写什么都没人接，这份输出**以后也拿不到**。
- `agent_refused` 的定义是"Agent 答复了拒绝"——Agent 确实答复了（"这次运行还没结束"，以后也不会有结局），调用方该得出的结论（别等、别重试取输出）也对。

所以值不改，只把协议第 9 节的举例补全：Agent 答复了、但交不出完整内容、等一会儿也不会变好，例如旧版本、会话目录已清理、**这次运行在 Agent 上没有留下结局、原始输出在第一次被读之前就不在了**。这是对已有含义的说明，不是改含义。排错手册同一节也改了，不再说"那个标签不对"。

## 4. F23：跑过的会话缺原始输出，如实降级

**缺陷**：Operator 第一次读 `session.result` 时才找 Agent 拷输出（P59 的做法）。Agent 建视图时，原始输出文件 `NotFound` 一律当成 0 字节。真机上会话结束后、第一次读之前把 Agent 上的 `stdout` 挪走，调用方拿到 `bytes_total: 0`、`complete: true`、`unavailable_reason: null`——等于说"输出就是空的，你已经拿全了"。

**为什么"不在"能判成"丢了"**：监督进程在启动 Agent **之前**就建好 `stdout`、`stderr` 两个文件；任何一步失败，结局里会有 `error`（"没起来"）。所以结局里没有 `error` 的会话，两个文件一定被建过。精确 stop 自己写的结局（`stopped: true`）也满足：它要先拿到 Agent 的 pid 才会写，而 pid 是在建文件之后才有的。

**改法**：`view::ensure` 多一个 `ran` 参数，`work::output` 传 `outcome.error.is_none()`。`ran` 为真而文件不在，拒绝建视图，返回 `CCNM_E_INTERNAL`（说明哪个流不见了），什么都不留下。Operator 侧不用改：P59 起拷贝失败就退回旧尾部，Agent 答复了错误（不是连不上）就标 `agent_refused`，而且不留快照，下次读还会再问一次 Agent。

不变的：

- 从没启动的会话（没登录、被提前停掉）本来就没有输出文件，照旧给空视图、完整。这条在旧代码上就是绿的，留着守住修复不误伤。
- 视图建好之后就不再依赖原始文件，之后原始文件丢了也照常翻页（新补了断言）。
- 文件在、只是空的：那是 Agent 什么都没打印，照旧是空且完整。

**回归**：

| 用例 | 修之前（`5e0c580`） | 修之后 |
| --- | --- | --- |
| `agent_output_refuses_a_lost_stream_of_a_session_that_ran`（`crates/ccnm-core/tests/session_identity.rs`） | 红：`Ok(OutputReport { view_bytes: 0, source_bytes: 0, source_truncated: false, … })` | 绿 |
| `agent_output_of_a_session_that_never_started_is_empty_and_complete` | 绿 | 绿 |
| `a_missing_stream_is_empty_only_for_a_session_that_never_ran`（`session::view`） | 编译不过（新参数） | 绿 |
| `out_a_stream_the_agent_lost_is_never_reported_complete`（`rpc::tests`，Agent 以 `INTERNAL` 拒绝时两次读都是旧尾部加 `agent_refused`、不留快照） | 绿（Operator 这一半本来就对，是护栏） | 绿 |

## 5. 门禁

本机负载 31–53（10 核）。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`；`cargo clippy --workspace --all-targets -- -D warnings`（1.98 与 `+1.99.0` 各一遍） | 通过 |
| `cargo test --workspace`（默认线程数） | 1068 通过（P67 后 1059，新增 9 条） |
| `cargo test --workspace -- --test-threads=64` | 1068 通过 |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 个 fixture）、Python 262 条 0 跳过 |
| `python3 scripts/check_protocol.py`、`python3 -m unittest tests.test_check_protocol`（改完协议说明后重跑） | 通过 |
| `git diff --check` | 通过 |

两次在旧代码上跑红的用例都靠各自的守卫收掉了测试目录，这一轮没有留下残留。

## 6. 日用两台换装（不属于 P68 的验收）

用户要求"升级本机工具"。本机日用的 ccnm 是 Runtime / Operator（`this = "runtime"`，四个 workspace），Agent 是 fodelf；P62 实测过新 Operator 对旧 Agent 会报 `CCNM_E_VERSION`，而 F22/F23 修的又都在 Agent 那端，所以问过用户后两台一起换成 P68 的构建。**这不是发版**：版本号仍是 0.10.1，和发布版 v0.10.1（hpsrv ccrun 上那份）不是同一个构建；P68 没有新增内部协议号，两种 0.10.1 混装时 doctor 分不出来。

| | 本机 xdwmbp | fodelf |
| --- | --- | --- |
| 换装前 | 0.9.0，`300dbd1d…`，没有 Controller | 0.9.0，`300dbd1d…`，Controller pid 1075，没有在跑的会话 |
| 装上的 | `cargo build --release --locked -p ccnm-cli`（`bebc539`，arm64，ad-hoc 签名），`90c93c0fc33b70a9c86adff6f0f29f072e186740307db72215d4f00baf7601d6` | 同一个文件，`scp -p` 过去 |
| 方式 | 新文件 `install` 到 `ccnm.new` 再 `mv` 盖上（不 `cp` 覆盖） | 同左；`controller install`（plist 与原来逐字相同），pid 1075 → 29110 |
| 备份 | `~/.local/opt/ccnm-0.9.0/ccnm`（核对 `300dbd1d…`） | `~/.local/opt/ccnm-0.9.0/ccnm` 与 `dev.ccnm.controller.plist` |

**核对**：本机对四个 workspace（`ccnm`、`xdo`、`xshun`、`gld`）跑 `ccnm doctor` 都是 0 项失败、3 项没查（Codex 原生链没开、本机工具策略、网络隔离，都是设计上不查的）；两端都报 `0.10.1`，反向 SSH、MCP 握手正常。零额度冒烟：`ccnm run ccnm --detached` 由新 Controller（pid 29110）起会话，Claude Code 2.1.289 直接停在输入框（没有信任提示），本机起了新二进制的 `mcp-serve`；不发消息，`ccnm stop ccnm --session <id>` 0.4 秒一次停下，`mcp-serve` 退出，fodelf 的 `ccnm log` 记"被停止"，本机四个写锁标记都是 `released`。没有调用模型。

**回退**（两台都要退，只退一边 doctor 会报版本不符）：

```bash
install -m 755 ~/.local/opt/ccnm-0.9.0/ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

fodelf 上同一条，再跑 `~/.local/bin/ccnm controller install`。

## 7. 没覆盖的

- 真机：2026-10-04 晚在 P62 拓扑上复验通过（监督进程被杀 1.2 秒后 `unknown`；挪走 `stdout` 后给旧尾部加 `agent_refused`），见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)。
- Linux：hpsrv 上 1072/1072（同上记录第 3 节）。Agent 目前只在 macOS 上跑（Controller 是 launchd），Linux 上的 `ps` 输出格式只由单元测试的字符串覆盖。
- 续跑记录 6.1 的余波没动：监督进程丢了的那次运行，`ccnm cleanup` 在三处都把它列成"说不清结束没有/没结束"而保留，只能等 7 天过期或手工删。（原先写的原因"Runtime 那边没有 `status` 文件"不对：正常结束的会话同样没有这个文件、照常可删，决定保留的是那次会话在 Agent 上没有结局，见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)第 6 节。）
- F20（doctor 对旧 Agent 先比身份）、F21（受管 Codex 会话 doctor 的 Command approval 行说错）、F24（`workspace add` 写死默认节点名）不在本阶段。
