# MCP 握手被拒时保留远端错误码（P25，2026-09-17）

结论：锁被占时从 Agent Node 起会话，现在报 `CCNM_E_POLICY`（退出码 33），不再报 `CCNM_E_RUNTIME_UNREACHABLE`（21）。传输命令和远端 stderr 仍在消息里。真连不上 Runtime 的情况分类不变。协议文档没改，因为两份冻结契约本来就要求这样报。

## 一、现象和根因

P24 真机轮（[记录](p24-native-real-machine-2026-09-16.md)第五节）：中立 `exec-serve` 握着写锁时 `ccnm run p24` 报

```text
CCNM_E_RUNTIME_UNREACHABLE:
MCP initialize failed over `/usr/bin/ssh … internal mcp-serve --payload …`: connection closed: initialize response
stderr: CCNM_E_POLICY:
workspace write guard is busy; another session still owns this working tree
```

`crates/ccnm-core/src/mcp/probe.rs` 在 `initialize` 失败时一律用调用方传进来的 `unreachable` 码包错误，不看 stderr。Runtime 侧的 `mcp-serve` 在握手前拒绝时，stderr 第一行就是它的 `CCNM_E_*`，这个码被丢掉了。`ssh.rs` 的 `remote_failure` 对一次性远端命令早就按首行保留码，握手这条路没跟上。

## 二、改之前核对的东西

**`probe()` 的三个调用方**，改完各自的变化：

| 调用方 | 谁用 | 原来的码 | 远端拒绝时现在的码 |
| --- | --- | --- | --- |
| `work::provider_runtime_preflight` | Agent 侧 `ccnm run`（交互与 `internal agent-run`），以及 Runtime 侧 `ccnm run --print` 经 ssh 调到的 Agent | `RuntimeUnreachable` | 远端的码 |
| `work::mcp_handshake` | doctor 的 `Remote MCP handshake` 与 `Project instructions` 两行（两侧 doctor 都经 Agent 跑这一步）、Agent 侧 `ccnm mcp probe`、Runtime 侧不带 `--local` 的 `ccnm mcp probe`（经 Agent 的 `internal probe`，`ErrorReport` 原样带回码） | `RuntimeUnreachable` | 远端的码 |
| `launcher::mcp_probe_local` | Runtime 本机 `ccnm mcp probe --local`，子进程就是本机 `mcp-serve` | `Internal` | 远端的码 |

`work::native_runtime_preflight` 不走 `probe()`，走 `Ssh::exec_transport_preflight`，那里本来就经 `remote_failure` 保留码，没改。锁被占时 MCP 预检先失败，走不到它。

**doctor 那两行**：`mcp_row` 和 `selected_project_instructions` 对握手错误都只做 `Check::fail_report`，行名、状态（FAIL）、结构都不变，只是冒号前的码变了；`doctor.rs` 里没有任何逻辑按握手错误的码分支。整份报告的退出码取第一个 FAIL 的码，所以锁被占时 doctor 退出码可能由 21 变 33。

**协议契约**：

- 机器协议（[machine-protocol-v1.md](../protocol/machine-protocol-v1.md) 第 10 节）：`-32005 runtime_unreachable` 是"Agent 到 Runtime 的 SSH 不通"，`-32007 policy` 是"被安全策略拒绝"。原来的行为把策略拒绝发成 `-32005` 才是和契约不一致。而且 `session.start` 先回句柄、后台才跑，后台失败在 `rpc/session.rs` 的 `spawn_run` 里只记 `err.message()`、不记码，`session.result` 也不输出它，所以 RPC 线上可见的东西都没变。`-32008 busy` 仍然不可达，[协议说明](../protocol/README.md)那一条照旧成立。
- Remote Workspace MCP（[remote-workspace-mcp-v1.md](../protocol/remote-workspace-mcp-v1.md) 第 11.3 节）：写入互斥 busy/unknown 本来就列在 `CCNM_E_POLICY` 下，`CCNM_E_RUNTIME_UNREACHABLE` 只表示 SSH 不通。那一节说的是 `ccnm mcp bridge`，bridge exec 成 ssh、不经过 `probe()`，本来就是远端原样的首行。
- `tests/`、`clients/` 下的 Python 客户端和 fixture 没有依赖预检错误码的地方（`grep RUNTIME_UNREACHABLE` 只命中协议 fixture 自己那两份"SSH 不通"样例）。

## 三、实现

- `ErrorCode::from_first_line(stderr)`（`crates/ccnm-core/src/error.rs`）：去掉开头空白后取第一行，去掉行尾空白，必须以 `:` 结尾且名字认得。`ssh.rs` 原来的 `first_line_is_ccnm_code` 删掉，两处调用和 `remote_failure` 都改用它。`remote_failure` 唯一的差别：首行带行尾空格时，原来判 `Internal`，现在认得出码——和它的调用条件（原来的 `first_line_is_ccnm_code` 本来就 trim）对齐了。
- `probe.rs`：`initialize` 失败时先把 stderr 读完，`from_first_line` 有码就用它，没有才用 `unreachable`；消息格式不变。**按完整 stderr 判，不按截到 4 KiB 的尾巴判**，远端解释写长了首行会被挤出尾巴。
- 只读首行是有意的：第二行以后出现的 `CCNM_E_*` 可能是别人引用的文字。代价是 ssh 在首行前插一句自己的话（比如首次连接时的 `Warning: Permanently added …`）就认不出码，照旧报 `RuntimeUnreachable`——和 `ssh.rs` 一次性命令那条路的行为一致，没有在这一阶段放宽。

## 四、测试（先红后绿）

新增两条经真实二进制的集成测试（`crates/ccnm-cli/tests/instance_execution.rs`）：Runtime 侧起一个真实 `internal mcp-serve`（protocol 4 的 `OpenPayload`）并等它回答 `initialize`，确保锁已占；Agent 侧真实 ccnm 经假 `ssh` 打到同一份 Runtime 配置和状态上的真实 `mcp-serve`。

- `a_busy_write_guard_fails_the_run_preflight_as_policy_not_unreachable`：`internal agent-run`，Controller 由测试线程应答两次（上下文、登录状态）；断言退出码 33、stderr 首行 `CCNM_E_POLICY:`、正文带 `MCP initialize failed over` 与 `internal mcp-serve` 与 `write guard is busy`，stdout 为空，没有建 session 目录。
- `a_busy_write_guard_fails_the_agent_side_mcp_probe_as_policy`：Agent 侧 `ccnm mcp probe demo --calls 1`，同样的断言。

**修复前**（HEAD 57658de 加上测试）：两条都失败，`left: Some(21)` / `right: Some(33)`，stderr 与 P24 真机上看到的逐行一致（首行 `CCNM_E_RUNTIME_UNREACHABLE:`，`stderr: CCNM_E_POLICY:` 之后是 busy 那段）。**修复后**两条通过。

单元测试：`error::tests::from_first_line_reads_only_the_first_line`；`mcp::probe::tests` 新增三条——远端拒绝保留码且消息带传输命令、stderr 超过 4 KiB 时码仍认得出、首行是 ssh 报错而第二行是 `CCNM_E_POLICY` 时仍报 `RuntimeUnreachable`。原有的"不是 MCP server 的进程报 unreachable""spawn 失败报 internal""超时"三条没改、仍通过。

变异检查：把判定临时改成按尾巴判，`the_code_survives_a_stderr_longer_than_the_kept_tail` 如期失败，其余通过；已还原。

## 五、门禁

macOS 26.6.2 arm64，rustc 1.98.0：

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：765 passed / 0 failed（P23 时 759，净增 6：error 1、probe 3、CLI 2）
- `python3 -m unittest discover -s tests -p 'test_*.py'`：168 passed
- `python3 scripts/check_protocol.py` 与 `tests.test_check_protocol`（24）：通过；`python3 scripts/check_plan.py`、`git diff --check`：通过

## 六、没做的

- 没跑真机。已装在 fodelf 和本机的是 0.7.0，不含这个修复；真机复验要替换二进制，需要单独授权。已发布版本的行为写进了[排错手册](../troubleshooting.md)那一条。
- 没补 `session.start` 的占用预检，`-32008` 仍不可达。
- 超时、spawn 失败、ssh 自身失败的分类没动。
