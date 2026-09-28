# P58 精确会话控制与原子状态（2026-09-28）

范围见[会话控制方案](../plan/core-session-control.md)第 1 节，反证来自 [P57 记录](2026-09-28-p57-core-baseline.md)。本轮只做离线实现与回归：**没有连 hpsrv/fodelf、没有运行真实模型或真实 Controller、没有部署或推送。** 真机结论留给 P62。

## 1. 结论

`session.stop` 现在只停它点名的那一个 Agent 会话。RPC 在派发前就给运行定好 Agent 上的 ccnm 会话 id 并落盘，内部请求（新协议号 7）把这个 id 带给 Agent，stop 也点名它；运行还没到 Agent 时，stop 先把 id 占住，运行到了就拒绝启动。会话记录的每次改动都在同一把文件锁下读改写，stop 标志不再被写回 `false`，已写下的终态不再被 `stopping` 覆盖。`start_key` 按原串比较，session 句柄在碰文件之前按协议形状校验，改绑 workspace 后旧句柄的 stop 不会发往新机器，派发后连接断开记为 `unknown`。

| 验收 | 结果 | 证据（本记录第 3 节） |
| --- | --- | --- |
| P58.1 反证先红后绿 | 最终的 10 条黑盒回归对旧代码全红，对新代码全绿；P57 探针在新构建上 A1–A8、B 全部翻转为符合 | 3.1、3.2 |
| P58.2 精确绑定 | 运行与 stop 用同一个 Runtime 分配的 id；Agent 侧三种窗口（未到、已建未启、已启动）各有确定结果，从不查 tmux | 3.3 |
| P58.3 原子状态与边界 | 64 线程并发更新零丢失；唯一临时文件；句柄校验在读文件之前；键无损；记录先于键落盘 | 3.3 |
| P58.4 终态与效应 | 终态不被覆盖；派发后的传输失败或内部错误为 `unknown`，启动前拒绝为 `failed` | 3.3 |
| P58.5 回归、门禁与文档 | Rust 973 条（64 线程）、Python 229 条、fmt/clippy/协议检查通过；旧版 0.9.0 对新请求以 `CCNM_E_VERSION` 拒绝；协议说明、运维、支持矩阵已同步 | 3.4、4 |

## 2. 改了什么

| 位置 | 改动 | 为什么 |
| --- | --- | --- |
| [rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs) | 所有写入走 `update`：在 `rpc/store.lock` 上 `flock`，读磁盘上的当前记录、改、以唯一临时文件加 rename 写回；新记录用 hard link 独占创建 | 运行线程和 stop 各持一份副本整份写回，后写的抹掉先写的（P57 A2/A3）；固定 `<id>.tmp` 也会被两个写者互相截断 |
| 同上 | `valid_handle`：1–128 字符、字母数字开头、只含 `[A-Za-z0-9._-]`；记录不是普通文件（symlink 等）就拒；错误里只写句柄不写路径 | `../x`、绝对路径能读到 `sessions/` 以外的文件（P57 A6），错误消息里带私有绝对路径 |
| 同上 | `start_key` 存进 `rpc/start-keys/<fnv1a>.json`，桶里保存原串逐字节比较；旧布局 `rpc/keys/<ws>/<key>` 只读：它指向的记录确实带这个键才算数，空文件是“无法确定” | `safe_name` 丢字符、截 64，不同键被合并（P57 A5）；升级后不能把已接受的任务当新任务重跑 |
| [rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) | 记录新增 `managed_session`（派发前生成）与 `dispatched`（派发前在锁内置位）；先写记录再占键，占不到就撤回自己的记录 | stop 必须在运行回话之前就能点名；键指向的记录必须已经存在 |
| 同上 | stop：终态幂等；无 `managed_session`（P58 之前的记录）拒绝 `-32000`；当前绑定的节点与记录不同拒绝 `-32007`；未派发则置 stop 并让运行线程不再派发；已派发则精确 stop，成功后才记 stop | 不按 workspace 猜；没送出去的 stop 不算请求过 |
| 同上 | 派发失败的分类：`AgentUnreachable`、`Internal` → `unknown`，其余（启动前的拒绝）→ `failed` | ssh 在运行开始后断开、Agent 启动后出错都可能留下运行，`failed` 会诱导重试 |
| [protocol/run.rs](../../crates/ccnm-core/src/protocol/run.rs)、[instance.rs](../../crates/ccnm-core/src/instance.rs) | `RunRequest.session`、`StopRequest.assigned`，两者都要求协议号 `ASSIGNED_SESSION_PROTOCOL = 7` | 旧 Agent 必须明确拒绝，而不是用自己的 id 启动一个没人能点名停止的会话 |
| [launcher.rs](../../crates/ccnm-core/src/launcher.rs) | `run_print_assigned`、`stop_assigned`；回答的身份或会话 id 对不上时报 `Internal`（不是 `Version`） | Agent 回话时可能已经做了事，不能被当成“什么都没发生”的拒绝 |
| [session.rs](../../crates/ccnm-core/src/session.rs)、[work.rs](../../crates/ccnm-core/src/work.rs) | `fence`（id 未建时占住并留 `stopping`）、`Control`（会话目录内 `control.lock`，运行从查标记到写 supervisor pid 期间持有）、`create` 改为 `create_dir` 独占；Agent 的精确 stop 按“未建 / 已建未启 / 已启动”三种处理，已启动走原有的 supervisor 校验 | print 运行不在 tmux；无 session 的 stop 只查 tmux（P57 B1/B2） |
| [paths.rs](../../crates/ccnm-core/src/paths.rs) | `fnv1a` 从 write_guard 挪来共用，行为不变 | 两处用同一个桶函数 |

对调用方可见的行为变化（都在冻结契约之内，已写进[协议说明](../protocol/README.md)）：stop 精确；未派发的 stop 让运行永不发出、终态 `failed`；非法句柄 `-32602`；改绑后 `-32007`；P58 之前的未完成记录 stop 回 `-32000`；派发后连接断开为 `unknown`；工作区被删后旧句柄 stop 由 `-32001` 变为 `-32009`（契约“未配置的一律 not_found”）；`start_key` 长度按字符数计（原来按字节，比 schema 的 128 字符更严）；`session::create` 撞上已有目录由 `CCNM_E_INTERNAL`（消息含私有路径）变为 `CCNM_E_NOT_READY`（只写 id）。

## 3. 验证

环境：macOS 26.6.2 arm64，rustc/cargo 1.98.0，Python 3.12.12。

### 3.1 黑盒回归先红后绿

[tests/test_rpc_exact_control.py](../../tests/test_rpc_exact_control.py) 只走真实二进制的字节协议，不 import ccnm；对面的 Agent 由 [fake_agent_ssh.py](../../tests/fixtures/fake_agent_ssh.py) 冒充，时序用放行文件控制。10 条用例对应 CT-01、CT-03（三条）、CT-04、CT-05（两条）、CT-06、CT-07、CT-08。

| 被测二进制 | 命令 | 结果 |
| --- | --- | --- |
| 旧代码：把 `crates/` 的改动临时 `git stash`、用 `fc1faaf` 的源码构建，测试文件是最终版 | `python3 -m unittest tests.test_rpc_exact_control` | **10 条全失败**（子用例计 25 failures + 3 errors），45.8 秒；失败点：stop 请求无会话 id、stop 标志回退、终态被改回 `stopping`、键合并、句柄越界读、改绑后 stop 发往新节点、空键回空串句柄、连接中断记 `failed` |
| 新代码 | 同上 | **10 条全过**，3.4 秒 |

stash 在红跑之后立即 `pop`，`git stash list` 为空；随后重新构建并复跑为绿。两次运行后 `/tmp/ccnm-exact-*` 与假 Agent 进程均为 0。

### 3.2 P57 探针复验

P57 的 [p57-rpc-control.py](probes/p57-rpc-control.py) 在新构建上重跑。为了让它能跑，本轮改了三处探针代码：假 Agent 按请求里的会话 id 回话（真实 Agent 就是这样），列键目录时兼容新布局，A7 在“一个 stop 都没发出”时不再越界取值。判定逻辑没改。

| 项 | P57（旧构建） | P58 构建 |
| --- | --- | --- |
| A1 停 B 的请求 | 与停 A 逐字节相同，无会话身份 | 不同，各自带 Agent 会话 id |
| A2 stop 后运行结束 | `completed` / `stop_requested: false` | `completed` / `stop_requested: true` |
| A3 终态先落盘 | stop 后读成 `stopping`，finish 丢失 | stop 回 `completed`，finish 保留 |
| A4 两进程 40 轮 | 39/1、36/4、37/3，无一次两者都保住 | 40/40 都是 `completed` + stop 标志 + finish |
| A5 不同键 | 合并，9 次 start 起 3 个 | 分开，起 6 个 |
| A6 越界句柄 | 读到 `sessions/` 以外 | `-32602`；symlink 记录 `-32603` 且不含路径 |
| A7 改绑后 stop | 发往 `worker2-alias` | `-32007`，一个 stop 都没发 |
| A8 空键文件 | `-32011`，`data.session: ""` | `-32011`，无 `session` 字段 |
| B Agent 侧 | 无 session 的 stop 停不到 print，改停交互会话 | 请求带精确 id 与 `assigned`，Agent 占住 id、不调用 tmux |

### 3.3 单元与集成回归

| 文件 | 新增 | 覆盖 |
| --- | --- | --- |
| [rpc/store.rs](../../crates/ccnm-core/src/rpc/store.rs) 测试 | 9 条（删掉 3 条：`release_key` 已不存在；“未知 id 读作 None”和“键不越出目录”的断言并入新用例） | 64 线程并发 `update` 各追加一个字符，最终 64 个、无残留临时文件（CT-03）；独占创建；旧记录可读；非法句柄与 symlink（CT-06）；键原串比较、桶内共存、旧布局三种情况（CT-04/05） |
| [rpc/mod.rs](../../crates/ccnm-core/src/rpc/mod.rs) 测试 | 10 条 | FakeRuns 可按会话放行：ct01 精确 stop；ct02 派发前 stop 永不派发；ct03 标志保留、终态先落盘不被覆盖；ct04；ct05；ct06；ct07；ct08 六种错误码的分类；P58 之前的记录拒绝精确 stop |
| [work.rs](../../crates/ccnm-core/src/work.rs) 测试 | 5 条 | 精确 stop 先到 → 占住 id、零外部命令、随后的运行拒绝且零外部命令；已建未启 → 留标记；已启动 → 只调 `ps` 校验、从不调用 tmux；别的 workspace 的 id 拒绝；非法 id 与缺实例拒绝 |
| [session.rs](../../crates/ccnm-core/src/session.rs) 测试 | 3 条 | 被占的 id 无法创建且错误不含路径；控制锁在释放前排斥另一个打开者；只接受 `new_id` 形状的 id |
| [assigned_session.rs](../../crates/ccnm-cli/tests/assigned_session.rs) | 3 条 | 真实二进制：带分配 id 却报协议 3 或 8 → `CCNM_E_VERSION`；stop 先到 → 占住 → `agent-run` 以 `CCNM_E_NOT_READY` 拒绝，PATH 里的假 ssh 从未被调用；畸形 id 不碰文件 |

原有用例里语义变了的三条，改法是跟着行为走而不是重录：stop 用例断言 `StopAsk` 带的是记录里的受管 id；“结果里没有内部字段”断言受管 id 由 RPC 生成且 Agent 回的是同一个；FakeRuns 默认的“起不来”改为 `NotReady`（启动前拒绝仍是 `failed`，`AgentUnreachable` 现在是 `unknown`，由 ct08 专门覆盖）。

### 3.4 全量门禁

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | 通过 |
| `cargo test --workspace --locked --offline -- --test-threads=64` | 973 passed / 0 failed / 0 ignored（P57 基线 946，本轮 +27） |
| `python3 -B scripts/ci_gates.py` | check_plan、check_protocol 通过；Python 229 ran / 0 skipped / 0 failed（基线 219，+10） |
| `cargo test -p ccnm-core --lib rpc::`、`--test rpc`、`--test instance_execution`、`--test assigned_session` | 89 / 11 / 9 / 3 passed |
| `tests.test_blackbox_client`、`tests.test_execution_backend`、`tests.test_check_protocol` | OK |

旧端拒绝新请求：用本机已装的 `ccnm 0.9.0`（`~/.local/bin/ccnm`，P58 之前的 build）在临时 HOME/state 下执行 `internal agent-run` 与 `internal agent-stop`，payload 为协议 7 的请求。两者都以退出码 11、`CCNM_E_VERSION: message is not valid for protocol 1; ccnm versions probably differ` 拒绝，stdout 为空，没有创建 state 目录。所以 Runtime 升级、Agent 未升级时，`session.start` 会以 `failed` + `CCNM_E_VERSION` 结束，不会退回旧路径。

## 4. 边界与未覆盖

- **没有真实 Controller**：运行持控制锁期间经 Controller 启动 supervisor 的那一步只在代码与锁语义测试里覆盖。若 Agent 侧 `agent-run` 在 Controller 已启动 supervisor、pid 尚未写盘时被杀，stop 会看到“没有 pid”而留下标记、回报未启动，那个 supervisor 却可能在跑——窗口很小，但没有进程归属证据可以堵上，记在这里。
- **被占住的 id 会留在 Agent 的 `sessions/` 下**（只有 `stopping` 标记，没有 `session.json`）。`status`、`history`、`purge` 都跳过读不出的目录，不影响功能；清理属于 P61。
- **持久性**：记录文件 `fsync` 后才 rename，目录不 `fsync`。断电可能丢最后一次 rename，不会出现半截记录。
- **跨进程只验证了同一台机器**：两个 `ccnm rpc` 进程共用一个 state 目录时由 `flock` 串行；不同 state 目录本来就是两个互不相干的记录库。
- **Linux 未跑**：CI 的 Linux job 会跑 Rust 与 Python 全套，但本轮只有本机 macOS 结果；hpsrv 真机留给 P62。
- **写权（busy / held）诊断没动**：那是 P60。stop 返回 `stopping` 之后，写锁何时释放仍只由 Runtime 判断。
