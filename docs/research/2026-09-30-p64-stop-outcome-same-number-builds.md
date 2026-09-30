# P64 交互会话的停止确认与结局、同版本号不同构建（2026-09-30）

修的是 [P62 真机](2026-09-30-p62-real-machine.md)查出的两条：F4、F2。范围见 [ROADMAP P64](../plan/ROADMAP.md)。两条都先把真机现象改写成对旧代码失败的回归，再修。**只有离线证据**：本机 macOS 26.6.2 arm64，没有连 hpsrv/fodelf、没有跑模型、没有部署或推送。`ccnm.machine/1` 的线格式没变，也没有新增内部协议号（`hello` 的回答多了一个可选字段）。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P64.1 F4 stop 等通道退出再确认 | 杀掉终端后每 100 ms 看一次通往 Runtime 的 ssh 通道，最多 5 秒；期限内退出就一次确认，到期还在回 `CCNM_E_NOT_READY`、状态保持 stopping，`ps` 读不出来不等也不确认 | `226bcda` |
| P64.2 F4 被停止是独立结局 | 结局多一个 `stopped` 标记，时长从会话记录写下算到停止请求落盘；`ccnm log` 显示"被停止 / stopped"和真实时长；没起来的会话显示不变；旧记录照常读 | `226bcda` |
| P64.3 F2 同版本号不同构建 | `hello` 回报 `wire`（这个构建的内部协议最高号，当前 10）；doctor 的两行版本检查和起会话前的握手在版本号相同时再比它，对不上或没报给 `CCNM_E_VERSION` 并说 `not the same build`；源码里有协议号超过它时测试红 | `30f4549`、`106316e` |
| P64.4 门禁与文档 | fmt、clippy、Rust 1033、`ci_gates.py`（计划、协议、Python 256 条 0 跳过）通过；排错、支持矩阵、README、P62 发现表、状态同步 | 见第 4 节 |

跑门禁时撞到四处偶发失败，都不是 F4/F2 引入的。其中一处是 P63 留下的**产品缺陷**（会话结束的那一刻 `session.status` 可能回 `unknown`），已先红后绿修掉；两处是测试自己的问题，已修；一处在后台命令的测试里，只在机器负载很高时出现，没动。见第 5 节。

## 2. 两条各改了什么

### 2.1 F4 前一半：stop 只看了一眼

**现象**（P62）：`ccnm stop <ws> --session <id>` 停 Codex 的交互会话，3 次里 3 次第一次都报

```text
CCNM_E_NOT_READY: terminal ended but its Runtime MCP transport is still alive; state remains stopping
```

几秒后再 stop 才说"没什么可停的"。Claude 的会话那一次第一次就成功。

**原因**：stop 让 tmux 杀掉会话，tmux 一说"没了"就立刻用 `ps` 找这个会话通往 Runtime 的那条 ssh（MCP 通道，模型的全部工具都走它）。终端没了，ssh 要再过一小会儿才退；只看一次，看到就下结论。和 P63 修的 F17（打印会话发完 SIGTERM 立刻查进程组）是同一个毛病。

**改法**（[work.rs](../../crates/ccnm-core/src/work.rs) `transport_ended_within`）：每 100 ms 看一次，最多等 5 秒——和 P63 给打印会话定的是同一组数（`STOP_GRACE`、`STOP_POLL`）。期限内没了就确认；到期还在，照旧回 `CCNM_E_NOT_READY`、`stopping` 标记留着、不写结局。`ps` 读不出来（返回"不知道"）**不等**：再问几遍它也不会变得可读，而一个没人核对过的答案不能当确认。

### 2.2 F4 后一半：被停止读成了"没起来"

**现象**（P62）：不管哪个 Provider，被 stop 停掉的交互会话在 `ccnm log` 里都是 `failed to start`、时长 `<1m`。Claude 那次实际跑了 7 分钟。

**原因**：tmux 杀会话时把 supervisor（守着 Agent 进程、负责在它结束时写结局的那个 ccnm 进程）一起带走了，没人写结局，所以由 stop 自己写。它借用的是 `record_terminal_failure`——那是"Agent 没能启动"的结局：`error` 有值、时长恒为 0。`log` 见 `error` 有值就显示"没起来"。

**改法**：
- [`session::Outcome`](../../crates/ccnm-core/src/session.rs) 多一个 `stopped` 字段，`record_stopped` 写它：`exit_code` 空、`error` 空、`stopped: true`、带真实时长。`stopped` 为 false 时不上线（不出现在 JSON 里），所以 P64 之前写下的记录读出来和以前一样。
- 时长 = 会话记录（`meta`）写下的时间 → **停止请求落盘的时间**（`stopping` 标记的修改时间）。不算到"现在"：第一次 stop 没确认、过了两分钟才再 stop 一次的，会话不该因此多算两分钟。
- 三处改用它：交互 stop 确认之后；第二次 stop 发现终端已经没了、而 `stopping` 标记还在（说明是上一次 stop 杀的）；打印会话里 supervisor 没来得及写结局的那条兜底路径。
- [`overview::render_history`](../../crates/ccnm-core/src/overview.rs)：`stopped` 显示"被停止 / stopped"，排在"没起来"之前判断。`Outcome::describe`（`ccnm result` 用）显示 `stopped by ccnm after N s`。

**没改的**：终端不是 stop 停的、而是自己消失（tmux server 被杀、机器重启），之后才有人 stop——没有 `stopping` 标记，ccnm 不知道它何时结束，仍记 `no managed terminal was running when ccnm stopped this session`，`log` 仍显示"没起来"。这个标签对它同样不准，但它不是 F4 的现象，没在本阶段动。会话状态的线上枚举（`SessionState`）也没加新值，被停止的会话在 `status` 的记录里仍归在 `failed` 一类。

**行为变更**（不是等价重构）：`session_identity.rs` 里两条旧断言原来检查 `error` 含 `stopped by ccnm` / `print process group ended`，现在检查 `stopped == true` 且 `error` 为空。

### 2.3 F4 的先红后绿

[`crates/ccnm-core/tests/session_identity.rs`](../../crates/ccnm-core/tests/session_identity.rs) 新增 5 条。前 4 条用脚本化的命令输出（`FakeRunner`），写完先在旧代码上跑：

| 用例 | 旧代码上 |
| --- | --- |
| `exact_stop_waits_for_the_runtime_transport_instead_of_looking_once`（`ps` 前两次还看得到通道，第三次没了） | `Err(NotReady, "terminal ended but its Runtime MCP transport is still alive; state remains stopping")` |
| `exact_stop_gives_up_after_the_grace_and_never_confirms_what_it_cannot_see`（通道一直在；另一段 `ps` 失败） | `gave up after 529.042µs, without waiting` |
| `a_stopped_session_is_logged_as_stopped_with_how_long_it_ran`（会话记录写在 7 分 20 秒前） | log 行是 `<1m  demo  failed to start` |
| `a_stop_confirmed_on_the_second_call_is_still_a_stop_at_the_time_it_was_asked`（`stopping` 标记在 2 分钟前，终端已没） | 同上：`<1m … failed to start` |

修后四条全绿，后两条的 log 行分别含 `stopped` + `7m`、`stopped` + `5m`。

第 5 条 `exact_stop_against_a_real_terminal_waits_out_a_transport_that_lingers` **什么都不造假**：真实 tmux（3.7c）、真实 `ps`、一个命令行上带着该会话 payload、终端被杀后拖 1.5 秒才退的 Python 进程当通道的替身。它证明脚本化用例假设的那几件事是真的——`ps -Awwo command=` 确实那样显示、tmux 确实在它还没退时就报会话没了、循环确实是因为进程真的退出才结束。修后这条用时 1.72 秒；把等待临时改成 0 秒（等于旧行为）时，它和第 1 条一起红，报的就是真机上那一句。

这条用例不碰日用的 tmux：ccnm 固定用 `tmux -L ccnm`，测试交给它的"tmux"是一个包装脚本，把 `TMUX_TMPDIR` 指到 `/tmp/ccnm-f4-<pid>-<随机>/`，用完 `kill-server` 并删目录。没装 tmux 的机器上它打印一行 `skipped` 后返回；在 CI（环境变量 `CI` 有值）上没装则直接失败——`ci.yml` 两个平台本来就装了 tmux。

`overview.rs` 另有一条单元测试固定四种显示：被停止（`stopped`、`7m` / "被停止"、"7 分钟"）、没起来、被信号杀掉、P64 之前的停止记录（仍是 `failed to start`）。

### 2.4 F2：版本号一样，构建不一样

**现象**（P62）：main 编出来的候选构建和已装的 v0.9.0 都报 `0.9.0`。旧 Agent 配新 Runtime：doctor **0 失败**，第一次起会话才被拒——`message is not valid for protocol 1; ccnm versions probably differ … unknown field session`。

**原因**：两端的一致性检查只比 `CARGO_PKG_VERSION`。版本号是发版前才升的，所以两次发版之间从 main 编出来的每个构建都叫上一个发布的号；而这段时间里内部协议从 6 加到了 10。

**改法**：
- [`payload::WIRE_LEVEL`](../../crates/ccnm-core/src/protocol/payload.rs)：这个构建的内部协议走到几，不低于 crate 里最大的 `*_PROTOCOL` 常量，当前 10。它不是某条消息的 `protocol`，没有任何分派看它。
- [`HelloReport.wire`](../../crates/ccnm-core/src/protocol/hello.rs)：`internal hello` 的回答里多带这一项。可选字段：旧构建解得开新回答（它不拒未知字段，v0.9.0 的源码核对过），新构建也解得开旧回答——解开之后才认得出对方是旧的。
- doctor 的 `Agent ccnm`、`Reverse SSH` 两行（[`version_row`](../../crates/ccnm-core/src/doctor.rs)）和 Agent 起会话前问 Runtime 的那次握手（`work.rs` 的 `greet`）：版本号不同照旧报；版本号相同再比 `wire`，对不上或对方没报，给 `CCNM_E_VERSION`：

  ```text
  work reports ccnm 0.9.0 like this machine, but it is not the same build: it does not say how far its internal protocols go, so it is older than this build; this machine speaks up to 10
  install the same build on both
  ```

  对方报了但不同时，中间那句是 `it speaks internal protocols up to 6`。

**先红后绿**：
- `doctor::tests::the_same_version_number_from_a_different_build_is_named_not_passed`：把一份全绿的探测结果里 Agent 的 `wire` 去掉（就是旧构建的回答），旧代码 `left: Ok, right: Fail(Version)`；另一段把 Runtime 的改成 6，检查 `Reverse SSH` 行。
- `work::tests::a_session_is_not_started_against_the_same_number_from_another_build`：旧代码直接放行（`must not get a session: ()`）。
- 喂的形状不是编的：本机已装的 0.9.0（sha256 前缀 `300dbd1d`，就是 P62 里当"旧 Agent"的那个）`internal hello` 实际回 `{"protocol":1,"ccnm_version":"0.9.0",…,"root":null}`，没有 `wire`；本轮构建回的多一项 `"wire":10`。这一步只读，没有动那个二进制。
- `hello::tests::the_wire_level_has_not_been_passed_by_a_protocol_number` 读 `crates/ccnm-core/src/` 下全部 `pub const …PROTOCOL: u32 = N;`，有哪个超过 `WIRE_LEVEL` 就红。把 `WIRE_LEVEL` 临时降到 9 验证过：`CLEANUP_PROTOCOL is 10, past WIRE_LEVEL 9: raise WIRE_LEVEL in the same change`。

## 3. 用的人看得到的变化

- `ccnm stop --session` 停交互会话，正常情况下一次成功；最多多等约 5 秒。
- `ccnm log` 里被 stop 停掉的会话是"被停止"加真实时长；`ccnm result <ws> --session <id>` 是 `stopped by ccnm after N s`。
- 两台机器版本号一样但不是同一个构建时，**新的那一端**跑 doctor，`Agent ccnm` 或 `Reverse SSH` 行失败（退出码 11）。Agent 是新构建、Runtime 是旧构建时，`ccnm run` 也在建会话之前就以同样的理由停下；反过来（旧 Agent、新 Runtime，P62 撞到的那种）起会话仍是被 Agent 拒绝，见第 6 节。
- **P64 构建和 P63 及更早的 main 构建互相配不上了**（对方不报 `wire`）。这是本意：它们本来就不是同一个构建。

## 4. 门禁

最终一轮（全部提交之后）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1033 通过 |
| `cargo test --workspace`（默认线程数） | 1033 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（43 + 29 个 fixture）、Python 256 条 0 跳过 |
| `python3 -m unittest tests.test_check_plan tests.test_check_protocol tests.test_ci_gates` | 48 条通过 |

P63 时 Rust 是 1022 条；新增 11 条：F4 6 条、F2 4 条、第 5.1 节 1 条。

环境：macOS 26.6.2 arm64，rustc 1.98.0，tmux 3.7c。跑完后 `/tmp/ccnm-f4-*`、`/tmp/ccnm-blackbox-*`、`/tmp/ccnm-backend-*` 都是 0 个，没有残留的替身进程；红测和偶发失败留在 `$TMPDIR` 下的目录（失败的用例会保留现场）已逐个删掉。

## 5. 跑门禁时撞到的偶发失败

四处，按"是不是产品的问题"排。

### 5.1 产品缺陷：会话刚结束时 `session.status` 回 `unknown`（P63 引入，已修）

**现象**：`test_rpc_disconnect` 的 `test_a_stop_after_hanging_up_still_reaches_the_run` 失败，`'unknown' != 'failed'`。单独重复跑 40 次 1 次。

**原因**：owner 进程（P63 起每个 Machine API 会话自己的那个 `ccnm internal rpc-run`）先写结局、再退出。`session.status` / `result` / `stop` 先读记录、再用 `ps` 查 owner 在不在。读在写之前、查在退出之后，拼出来就是"还在跑、owner 没了"，按规则回 `unknown`——而磁盘上的记录这时已经写着 `completed` 或 `failed`。P63 之前 owner 就是 `ccnm rpc` 自己，同一个进程读自己的记录不查 `ps`，这个窗口只在跨连接时才有；P63 之后每个会话结束的那一刻都有。

**为什么要紧**：`unknown` 是终态，意思是"可能已经改了东西，别重试，去现场看"。参考客户端的 `wait()` 拿到它就返回。也就是一个正常结束的任务，有几十分之一的机会被调用方当成说不清。

**改法**（`a1de669`，[rpc/session.rs](../../crates/ccnm-core/src/rpc/session.rs) `observe`）：五处判断统一成一个函数——查到 owner 不在了，把记录重读一遍再下结论。owner 不在，说明它要写的都已经落盘，这次重读不会再有窗口；重读后仍不是终态的才是真的被丢下，照旧 `unknown`。

**先红后绿**：`rpc::tests::an_owner_that_finished_between_the_read_and_the_ps_is_not_unknown` 用一个"被问到时先替 owner 把结局写好、再回答 pid 不存在"的 `ps` 把窗口做成确定性的。旧代码 `left: "unknown", right: "completed"`；新代码 `status`、`result`、`stop` 三条都回 `completed`，而且没有任何停止被发出去。原来偶发的那条 Python 用例改后单跑 150 次 0 次。

### 5.2 测试收尾和 owner 进程抢目录（已修）

**现象**：`test_blackbox_client` 和 `test_execution_backend` 各撞到一次 error：`OSError: [Errno 66] Directory not empty: '/tmp/ccnm-blackbox-…/state/ccnm/rpc/sessions'`，/tmp 下留一个只剩一份记录的目录。单跑 25 次约 1 次。

**原因**：这两个类用 `TemporaryDirectory`，用例一结束就删；owner 正好在这时写结局。留下的那份记录里写着 `config not found`——它启动时配置已经被删了。

**改法**（`6d58ac4`、`c54b7a3`，只动测试）：[tests/rpc_owners.py](../../tests/rpc_owners.py) 按记录里的 `owner_pid` 等它们退出（最多 30 秒，超时算失败），两个类都在删目录之前调它。改后两个文件一起连跑 25 遍 0 次，无残留。

**同一个原因的另一种表现**：Rust 集成测试 `crates/ccnm-cli/tests/rpc.rs` 里起了会话就返回的用例不会失败，但沙盒被删之后 owner 才启动到打开 store 那一步，又把 `state/ccnm/rpc/store.lock` 建了回来——`$TMPDIR` 下每跑几次多一个 `ccnm-rpc-it-<pid>-failed-start`（本轮留了 4 个，P63 那一轮留了 1 个）。`7d00840` 让这几条用例返回前等 owner 把结局写完；改后这个文件连跑 12 次没有新目录。

### 5.3 CT-01 赌 stop 先于运行结束回话（已修）

**现象**：`rpc::tests::ct01_a_stop_names_exactly_its_own_session` 在 64 线程下失败，stop 回的是 `failed` 而不是 `stopping`。只跑 `rpc::` 模块 30 次 3 次；在本阶段动 rpc 之前的提交上同样是 3/30。

**原因**：假执行器一收到 stop 就放那次运行结束，运行可能抢在 stop 回话前把终态落盘。两种回答都合契约，`stop_requested` 都是 true。`02b6a29` 给 Python 那条同类用例改过，Rust 这条漏了。

**改法**（`2da7eb9`）：断言改成"`stopping` 或 `failed`，且 `stop_requested` 为真"。改后同样的跑法 30 次 0 次。

### 5.4 后台命令的两条用例在高负载下偶发（没动）

`mcp::jobs::tests::a_background_command_returns_at_once_and_is_read_as_it_grows`（`the waiter left the registry`）和 `a_server_that_died_leaves_an_unknown_ending`（`r-… is running in another server of this session`）。本轮全量 64 线程一共跑了 10 次，有 1 次红了前一条；那之后 `cargo test -p ccnm-core --lib -- --test-threads=64` 连跑 6 次，2 次各红一条。这两段都紧跟在我几十轮连续压测之后，随后量到的负载是 22–30（平时这台机器是多少没有量）；最后 4 次全量 64 线程在负载 24–37 下都是全过，所以它是低概率的，不是必现。它们属于 P41/P42 的后台命令，和本阶段无关，两条的失败原因也没有查，所以没改，记在 `status.json` 的 `observed_gaps` 里。

## 6. 没覆盖的

- **真机没有复验。** 5 秒这个上限沿用 P63，依据是 P62 两台 Mac 上"几秒后就干净了"的观察，没有精确量过 Codex 的 ssh 通道到底拖多久；真实时序留给 P62 续跑。更慢的会得到 NotReady，再 stop 一次即可，结局仍是"被停止"。
- **Linux 没有在本轮跑。** 真实 tmux 那条用例依赖 `ps -Awwo command=` 和 tmux 杀会话时的信号行为，macOS 上验了，Linux 上要么授权后到 hpsrv 跑，要么推送后看 CI 的 ubuntu job。
- **F2 只修了新的那一端。** 已经发布的 v0.9.0 只比版本号，它看 main 上的构建仍然是"同一个 0.9.0"，doctor 全绿。要让旧的一端也分得清，只有版本号不同——也就是发版时升号（`release.yml` 本来就拒绝 tag 和版本号对不上的树）。本阶段没有升 workspace 版本号：历来是发版前才升，什么时候发、升到几由用户定。
- **Operator 到 Agent 的派发没有加握手。** Machine API 的 `session.start` 和 `ccnm run` 把请求发给 Agent 之前不先问一次 hello（那要多一次 ssh，实测 430–490 ms）；旧 Agent 配新 Runtime 时，会话仍然是被 Agent 以 `CCNM_E_VERSION` 拒绝。变化在于 doctor 现在会先说出来。Machine API 调用方看不到失败原因的问题是 F3，没在本阶段。
- `WIRE_LEVEL` 只在新增协议号时被测试逼着抬高。两个构建协议号相同、工具行为不同（P63 就是这样的改动）时，它们仍然比出"同一个构建"；要不要在这类改动里也抬高它，靠改的人判断。
- F1、F3、F5 与 F15 以后各项不在本阶段。
- `$TMPDIR` 下还有 12 个更早的轮次（09-28 到 09-30 下午）留下的 `ccnm-*` 目录，是那时失败的用例保留的现场，本轮没有动。
