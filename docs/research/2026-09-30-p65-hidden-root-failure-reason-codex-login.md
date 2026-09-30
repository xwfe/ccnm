# P65 Operator 看不进项目目录、Machine API 的失败原因、Codex 登录只看本地（2026-09-30）

修的是 [P62 真机](2026-09-30-p62-real-machine.md)查出的三条：F1、F3、F5。范围见 [ROADMAP P65](../plan/ROADMAP.md)。**只有离线证据**：本机 macOS 26.6.2 arm64，没有连 hpsrv/fodelf、没有跑模型、没有部署或推送，也没有碰任何登录文件。`ccnm.machine/1` 加了一个可选响应字段；没有新增内部协议号。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P65.1 F1 Operator 没权限看项目根 | "不在"和"不让看"分开：`ccnm run` 放行、doctor 记"没查"并指向执行账号那一行、`workspace add` 按写下的绝对路径登记并说明没核对、`workspace list` 与 `status` 不再标"不在这台机器上"；确实不在或不是目录的照旧拒绝 | `8756bd4` |
| P65.2 F3 会话没起来的原因 | `session.result` 多一个可选的 `failure`：第 10 节同一套 `code` / `ccnm_code` 加 `detail`；契约、schema、三个 fixture 先改，两份参考客户端同步；`detail` 里家目录前缀写成 `~`；旧记录只有 `detail` | `204400f`（契约）、`4cd596f`（实现） |
| P65.3 F5 Codex 登录只看本地 | doctor 的 `Codex authentication` 行仍是 OK，但写明"只看了本地登录状态、没向服务器核对"；**没有加联网探测**，候选命令和动手前要量的事见 2.3 | `8590d01` |
| P65.4 门禁与文档 | fmt、clippy、Rust 1045、`ci_gates.py`（计划、协议 46 + 29、Python 262 条 0 跳过）通过；协议实现说明、运维与排错手册、支持矩阵、README、交接文档、P62 发现表、状态同步 | 见第 4 节 |

## 2. 三条各改了什么

### 2.1 F1：不让看，不等于不在

**现象**（P62，hpsrv Debian 13）：照手册把项目放进执行账号 `ccrun` 的家目录。Operator（另一个账号）敲 `ccnm run p62rust`，退出 30：

```text
CCNM_E_WRONG_WORKSPACE:
workspace root /home/ccrun/… is not a directory on this machine, which is the Runtime Node for 'p62rust'
```

doctor 的 `Runtime workspace` 行是 `CCNM_E_INTERNAL: cannot stat …`。目录一直都在。

**原因**：Debian 12 起新账号的家目录默认 0700（`/etc/login.defs` 的 `HOME_MODE`），Operator 进不去，`stat` 得到 `Permission denied`。代码里用的是 `root.is_dir()`，它对"不在"和"不让看"都答 `false`。运维手册里早就记着的 `ccnm workspace add` 缺陷是同一个根子。macOS 的家目录默认别人能进，所以之前的真机轮没撞到。

**改法**：[`paths::see_dir`](../../crates/ccnm-core/src/paths.rs) 答四种——是目录、不是目录、不在、不让看（另有其他 I/O 错误）。"不让看"时，每一处按"这个账号没资格下结论"处理：

| 位置 | 以前 | 现在 |
| --- | --- | --- |
| 连 Agent 之前的本地检查（`ccnm run`、`attach`、`stop`、`result`、Machine API 的 `session.start` 等都经过它） | 退出 30 | 放行。项目是执行账号的，它在开会话时回答：Agent 握手会拿同一个路径问它，不在就报 `workspace <名字> says its root is …, and on that machine it is missing` |
| doctor `Runtime workspace` 行 | `FAIL CCNM_E_INTERNAL: cannot stat` | `SKIP`，写明谁看不了、谁的回答在哪一行（受管 workspace 是 `Workspace root` 行；只给外部 MCP 用的 workspace 没有那一行，写的是"外部客户端连进来时由执行账号回答"） |
| doctor `Project instructions` 行（旧式 `agent_node` workspace 才由 Operator 读） | `WARN`，说"会话将没有项目指令" | `SKIP`：读 `CLAUDE.md` 的是执行账号，这个账号说不了 |
| `ccnm workspace add` | 退出 30，`is not a directory … caused by: Permission denied` | 登记，并打印一段说明：没核对它在不在，也没解析符号链接 |
| `ccnm workspace list`、`ccnm status` | 标"不在这台机器上" | list 标"这个账号没权限看"；status 不再标 |

确实不在、确实不是目录的，行为和文案都没变。

**`workspace add` 为什么只接受绝对路径**：没法解析（`canonicalize` 需要逐级进入目录）时，只能照写下的字面登记。相对路径、带 `.` 或 `..` 的路径有不止一种读法，又没人能核对，所以照旧拒绝，并提示"写执行账号用的完整路径"。登记的路径如果经过符号链接，和执行账号自己解析出来的会不一样——说明文字里提了这一点，真对不上时会在开会话处被拒。

**先红后绿**（`crates/ccnm-cli/tests/cli.rs`，真实二进制）：在一个 `0000` 的父目录里放一个项目，三条用例对旧代码分别是

```text
run         退出 30：workspace root …/executor-home/proj is not a directory on this machine, which is the Runtime Node for 'xshun'
doctor      Runtime workspace       FAIL   CCNM_E_INTERNAL: cannot stat …/executor-home/proj
ws add      退出 30：…/executor-home/proj is not a directory on this machine / caused by: Permission denied (os error 13)
```

就是真机上的三句原话。新代码全绿；同一条用例里还断言了"真不在"和"是个文件"仍然退出 30、相对路径仍然被拒。`paths.rs` 另有一条单元测试固定四种回答。**以 root 身份跑时权限拦不住**，这几条会打印 `skipped` 后返回（CI 和 hpsrv 上的 ccrun 都不是 root）。

修后的实际输出（中文界面，路径做了替换）：

```text
Runtime 上的项目        没查   not checked: bing is not allowed to look at /sandbox/executor-home/proj (Permission denied), so this account cannot say whether the project is there
                               the account that runs the tools can: its answer is the `Workspace root` row below
项目指令                没查   not checked: bing is not allowed to look at /sandbox/executor-home/proj, and the session reads CLAUDE.md as the account that runs the tools
```

### 2.2 F3：会话没起来，调用方要知道为什么

**现象**（P62）：Machine API 的三种失败——Agent 上的 Claude 没登录、Runtime 被退回 0.8.0、旧 0.9.0 的 Agent 配新构建——到调用方手里都是同一个样子：`failed`，`exit_code` 和 `text` 是 null，stdout、stderr 各 0 字节。原因只在 Operator 自己的记录文件的 `finish.error` 里。

**原因**：协议 v1 没有给"Agent 进程没起来"留原因字段；实现把错误的那句话存了，错误码丢了。

**契约**（先改，`204400f`）：[协议 5.5 节](../protocol/machine-protocol-v1.md#55-sessionresult)加一个可选响应字段，按第 13 节属于加法：

```json
"failure": {"code": -32003, "ccnm_code": "CCNM_E_AUTH", "detail": "Claude is not authenticated on the Agent Node"}
```

- `detail` 必有，给人看；`code` 和 `ccnm_code` 可选，用的是第 10 节错误码表的同一套——**就是这次启动如果当场被拒会回的那个错误**。调用方已有的"按错误码分支"可以直接读它。
- 只在会话**不是以 Agent 进程自己结束收场**时出现：没起来（`failed`），或服务端跟丢了（`unknown`，这时说的是"为什么说不清"）。Agent 自己退出的（不管退出码）、被 stop 的、超时的都没有。**没有 `failure` 不代表成功。**
- 不改变状态的含义：`failed` 带着它仍是"没执行过"，`unknown` 带着它仍是"可能已经改了东西"。

schema 加 `session_failure`；新增三个 fixture：没起来（带码）、跟丢了（`unknown`）、派发前被 stop（只有 `detail`）。

**实现**（`4cd596f`）：
- 记录里的 `Finish` 多一个 `error_code`，运行失败时和那句话一起存。supervisor 报"没能启动"时是一句话，码从它开头读（两种写法：`CODE:` 独占一行，或 `CODE: 原因`）。
- "内部错误码 → 协议数字码"抽成 [`wire::code_of`](../../crates/ccnm-core/src/rpc/wire.rs)，被拒的调用和 `failure` 用同一张表；一条测试逐个错误码核对两边给的数相同。
- `detail` 是别人的原话（Agent 的拒绝、ssh 的 stderr），里面会有 profile、state 目录。出门前 `wire::public_detail` 把 `/Users/<名字>`、`/home/<名字>` 换成 `~`，并限制在 2048 字节内（按字符边界切，末尾加 `…`）。不在家目录下的路径（`/usr/bin/ssh`、`/srv/…` 下的项目）不动。
- P65 之前写下的记录只有那句话：`failure` 只有 `detail`。

**两份参考客户端**：[ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py) 补了三个会在 `failure.code` 里见到的码（`E_AUTH`、`E_AGENT_UNREACHABLE`、`E_RUNTIME_UNREACHABLE`）；[execution_backend.py](../../clients/python/execution_backend.py) 的 `ExecutionResult` 多一个 `failure`（`detail`、和 `BackendError` 同一套的 `kind`、原始 `code`），`FakeBackend.on(..., failure=…)` 能演"没起来"。

**先红后绿**：
- Rust `rpc::tests::a_session_that_never_started_says_why_in_its_result`（Auth、Version、RuntimeUnreachable 三种 `failed` 和 AgentUnreachable 的 `unknown`）、`an_older_record_gives_only_the_detail_and_home_paths_are_not_passed_on`：旧实现上 `failure` 是 `Null`。
- 新文件 [tests/test_rpc_failure.py](../../tests/test_rpc_failure.py)：真实 `ccnm rpc` 加假 Agent（`fake_agent_ssh.py` 新增 `refuse`，照真实 ccnm 拒绝时的样子把 `CODE:\n原因` 写到 stderr、以对应退出码退出），走 P62 见过的三种原话、带家目录的原因、连接断在中途、正常退出、被 stop。把 `crates/` 的改动收起来用旧实现构建时，6 处 `KeyError: 'failure'`；恢复后 5 条全过。
- 两条原来要去读 Operator 记录文件才能断言原因的黑盒用例（`test_blackbox_client`、`test_execution_backend` 里"到不了 Agent"那两条），改成从协议结果里取——对真实二进制，`failure` 是 `{"code": -32004, "ccnm_code": "CCNM_E_AGENT_UNREACHABLE", "detail": "… Could not resolve hostname …"}`。

### 2.3 F5：Codex 登录，doctor 只看得到"登录过"

**现象**（P62）：ccnm 专用 Codex profile 的刷新令牌已被吊销。`codex login status` 说 `Logged in using ChatGPT`，doctor 的 `Codex authentication` 是 OK，会话发第一条消息才报 `refresh token was revoked`。

**先查清官方 CLI 能做什么**（AGENTS.md：官方 Agent 的参数和行为以实测版本及 fixture 为依据，不猜）：

| 依据 | 看到的 |
| --- | --- |
| 仓库 fixture `tests/fixtures/codex-0.154.0/auth-help.json`、`help.json`（受管适配器 pin 的版本） | `codex login` 下只有 `status` 一个子命令，没有任何联网或校验的选项；顶层有 `codex doctor`："Diagnose local Codex installation, config, auth, and runtime health" |
| 本机 `codex --version` | 0.158.0（Homebrew）。**0.154.0 本机已经没有了**，P62 收尾时删的 |
| `codex doctor --help`（0.158.0） | 有 `--json`："Emit a redacted machine-readable report" |
| 在一个**空的**临时目录里跑 `CODEX_HOME=<空> codex doctor --json`（0.158.0，没有任何凭据） | 退出码 1；报告里有 `auth.credentials`（看的是 auth 文件和存储方式）、`network.provider_reachability`、`network.websocket_reachability` 等 14 项检查；其中一项联网检查回来的是 `Missing bearer or basic authentication in header`——也就是说有凭据时它很可能会带着令牌发请求 |

**没有量、也不该在没授权时量的**：`codex doctor` 在**有凭据**时会不会先刷新令牌。刷新就是改写登录文件、换掉刷新令牌；doctor 是只读的，一个会改写凭据的探测不能放进去。要量这个，得拿一份真实登录过的 profile 在 0.154.0 上跑，最好还有一份令牌已吊销的——那是动凭据的事，要单独授权。

**所以这一阶段只做前一半**（`8590d01`）：那一行仍是 OK——它查的那件事确实过了——但写明查的是什么：

```text
Codex authentication    OK     logged in via ChatGPT
                               local login state only, not checked with the server: a revoked or expired token still reads as logged in, and the first message of a session is what shows it
```

Claude 那一行不加这句话：`claude auth status` 问不问服务器同样没量过，编一句只是多一个没验证的说法。

**要接 `codex doctor` 之前必须先量的**（留给有授权的那一轮）：
1. 0.154.0 的 `codex doctor` 有没有 `--json`，输出形状和 0.158.0 是否一致。
2. 对一份有效登录：跑前跑后登录文件的修改时间和大小变没变（不读内容），也就是它刷不刷新。
3. 对一份已吊销的登录：它报不报得出来，报在哪一项、什么状态。
4. 它发的请求算不算模型额度（P62 那次额度用尽时顺便能看）。
5. 耗时，以及没网时多久返回。

**先红后绿**：`doctor::tests::the_codex_login_row_says_it_only_saw_the_local_login` 旧代码的 detail 只有 `logged in via ChatGPT`。

## 3. 用的人看得到的变化

- Linux 上项目可以放在执行账号的家目录里了：`ccnm workspace add <名字> /home/ccrun/<项目>`（写绝对路径）能登记，`ccnm run` 不再被挡，doctor 的 `Runtime 上的项目` 一行是"没查"而不是失败。doctor 本来就常年有几行"没查"，多这一行不改变它的结论和退出码的算法。
- Machine API：会话没起来时 `session.result` 里有 `failure`，按 `failure.code` 分支。老客户端不认识这个键，照协议忽略即可。
- `ccnm doctor --agent <codex 实例>` 的登录行多一行说明。
- P65 的构建和 P64 的构建内部协议没变（`WIRE_LEVEL` 仍是 10），互相配得上。

## 4. 门禁

最终一轮（全部提交之后）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1045 通过 |
| `cargo test --workspace`（默认线程数） | 1045 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 个 fixture）、Python 262 条 0 跳过 |
| `python3 -m unittest tests.test_check_plan tests.test_check_protocol tests.test_ci_gates` | 48 条通过 |

P64 时 Rust 是 1033 条；新增 12 条：F1 4 条、F3 7 条、F5 1 条。Python 从 256 到 262：`test_rpc_failure` 5 条、`test_execution_backend` 1 条。协议 fixture 从 43 到 46。

环境：macOS 26.6.2 arm64，rustc 1.98.0。红测留在 `$TMPDIR` 下的目录已逐个删掉；`/tmp/ccnm-*` 为 0。

**两次和本阶段无关的失败**，都出现在机器负载很高的时候（同一台机器上另有一个会话在反复跑测试查下面第一条）：

- 中途一轮 64 线程全量里 `mcp::jobs::tests::a_background_command_returns_at_once_and_is_read_as_it_grows` 红了一次（`the waiter left the registry`），随后量到的负载是 27–31。这是 [P64 记录](2026-09-30-p64-stop-outcome-same-number-builds.md) 5.4 记下的那条，那个会话正在查，本阶段没有动。之后 64 线程全量连过 3 次。
- 最终一轮的 `ci_gates.py` 第一次跑时，`test_remote_workspace_mcp.RemoteWorkspaceMcpTests.test_a_child_left_in_the_servers_process_group_ends_before_the_next_writer` 报了一次 error，负载 36–50。**当时只留了最后两行输出，报错原文没有保住**，所以不知道它具体卡在哪。这条单独跑 6 次、整套门禁连跑 4 次（负载 48–70）都没有再出现。它测的是 Runtime MCP server 的写锁交接，P65 没有碰那部分代码。记在 `status.json` 的 `observed_gaps` 里。

## 5. 没覆盖的

- **真机没有复验，Linux 没有跑。** F1 正是 Linux 上的事：权限模型在 macOS 上用 `chmod 000` 的父目录模拟了同一个 `EACCES`，但真实的"Operator 与 ccrun 两个账号、0700 的家"要到 hpsrv 上看；握手处由执行账号回答"不在"的那条路径也只有既有的单元测试，没有在这个布局下端到端跑过。留给 P62 续跑。
- **F5 没有变成真的校验。** 令牌被吊销时 doctor 仍然是 OK，只是不再让人以为它验过。
- **`workspace add` 在看不了时不核对、不解析符号链接。** 路径写错要到 doctor 的 `Workspace root` 行或开会话时才知道。
- **脱敏只做了家目录前缀，只做在 `failure.detail`。** state 或 profile 目录不在家目录下（自定义 `XDG_STATE_HOME`）时不会被换掉；被拒调用的 `error.message` 仍是 ccnm 给人看的原话，没有过这道处理——这是 P65 之前就有的差距，已写进[协议实现说明](../protocol/README.md)。
- **`session.status` 不带 `failure`**，只有 `session.result` 带。
- **Operator 到 Agent 派发前仍不握手**，所以"旧 Agent 配新 Runtime"还是起了会话才失败；区别是现在 `failure` 会说 `-32002`，而 doctor（P64）会事先指出来。
- F6 以后各项不在本阶段。
