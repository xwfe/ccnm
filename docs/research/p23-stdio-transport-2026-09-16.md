# Agent 侧接线：Codex 自带的 stdio 传输（P23，2026-09-16）

设计见[双执行入口方案](../plan/runtime-surfaces.md)第 12 节，验收见 ROADMAP 的 P23。Runtime 那一半（`ccnm internal exec-serve`）在 [P22](p22-exec-serve-2026-09-16.md)。本文记两件事：为什么 P23 没有按立项时的"WebSocket 网桥 + 查对端 uid"做，以及做成什么样、怎么验的。

**全程零模型额度**：模型接口是本机假服务（沿用 P21 的 `MockModel`），Codex 外面套 `sandbox-exec` 禁非本机出站，`HOME`/`CODEX_HOME` 是临时目录。没在任何真实 Runtime 上装东西，没换任何机器上已装的 ccnm。

## 1. 开工前的核对：Codex 不是只能走 WebSocket

用户问了一句"Codex 真的只能通过 ws 连通？"。答案是**不是**。Codex 0.154.0 的 exec-server 客户端有三种传输（`codex-rs/exec-server/src/client_api.rs` 的 `ExecServerTransportParams`）：

| 传输 | 怎么配 | 断线后 |
| --- | --- | --- |
| `WebSocketUrl` | `CODEX_EXEC_SERVER_URL=ws://…`，或 `environments.toml` 里的 `url` | 有 reconnect 策略，按 `resumeSessionId` 恢复会话（G05-peer 实测：网桥放行重连它就自己 resume） |
| `NoiseRendezvous` | `CODEX_EXEC_SERVER_NOISE_*` 环境变量，走 OpenAI 的中继 | 同上 |
| **`StdioCommand`** | `CODEX_HOME/environments.toml` 里写 `program`/`args`/`env`/`cwd`，Codex 自己 spawn 这个程序、拿它的 stdin/stdout 走同一套 JSON-RPC（`client_transport.rs` 的 `connect_stdio_command`） | **没有 reconnect 策略**（`client_recovery.rs`：`reconnect_strategy.is_none()` 直接 `fail`），进程放在自己的进程组，Codex 退出时整组终止 |

第三种正是 ccnm 要的形状——Codex 官方自己的单元测试里那个环境就叫 `ssh-dev`，`program = "ssh"`。它没写进 exec-server 的 README，只在源码和测试里，所以版本 pin 仍然是前提。

几条决定接线方式的源码事实：

- TUI 启动时先 `EnvironmentManager::prepare_from_codex_home(codex_home)`（`tui/src/lib.rs`）：`CODEX_HOME/environments.toml` 存在就按它配置环境，否则退回 `CODEX_EXEC_SERVER_URL`。`default` 指向远端环境时，本机工作目录检查跳过（`config_cwd_for_app_server_target`），这就是 P21 里 `-C <Runtime 根>` 能用的原因。
- `should_load_configured_environments`：`--ignore-user-config` 会**关掉** environments.toml。ccnm 的交互模式本来不传它（只有 print 传），而原生链只开交互模式；实测交互模式根本不认这个参数（`unexpected argument`）。
- `initialize` 带 `resumeSessionId: null`（`StdioExecServerConnectArgs.resume_session_id` 固定 `None`），正好过 P22 规则表。
- spawn 前只剥掉 5 个 Codex 自己的凭据类变量（`NON_INHERITABLE_ENV_VARS`），其余环境原样继承，所以传输程序看得到 ccnm 给 Codex 的 `CODEX_HOME`。
- 认证文件：`login/src/auth/storage.rs` 读用 `File::open`，写用 `OpenOptions::truncate().write().create()` **原地覆盖**，不是写临时文件再 rename——这决定了 symlink 能用（下一节）。
- 信任提示：`should_show_trust_screen` 看的是 `config.active_project.trust_level`。

## 2. 实测（脚本在 toexec [`evidence/v2-c/p23-stdio/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p23-stdio/README.md)）

第一批是开工前的 spike，用一个 Python relay 当 `program`，目的只是回答"Codex 会不会这样连"：

| 场景 | 结果 |
| --- | --- |
| `basic`：environments.toml 指定 relay，Codex TUI `-C <根>` | 传输程序被 spawn **1 次**，环境里带 ccnm 给的 `CODEX_HOME`，自己一个进程组、父进程是 Codex；`initialize` 参数只有 `clientName` 和 `resumeSessionId: null`；两条命令都在 exec-server 上执行，`cwd` 和 `workspaceRoots` 都是 `-C` 给的路径；模型收到真实输出 |
| `drop`：第一条命令结束后 relay 自杀 | Codex **没有**再起第二个传输程序；第二条命令报 `exec_command failed: ProcessFailed { message: "exec-server transport disconnected" }`，第二个文件哪里都没写出来；TUI 里显示同一句 |
| `login`：每会话 CODEX_HOME 里 `auth.json` 是指向 profile 的 symlink | `codex login status` 透过 symlink 读到 profile 的 key；`codex login --with-api-key` 写回去的是 **profile 那个文件**（0600 保留），symlink 本身还在 |
| `basic-notrust` / `basic`：不带、带 `-c projects."<根>".trust_level="trusted"` | 两种都弹信任提示——命令行覆盖压不住 |
| `basic-configtrust`：在每会话 CODEX_HOME 的 `config.toml` 里写同一条 | 不弹 |

顺带撞到一条：Codex 跑在 `sandbox-exec` 里时，作为它子进程的 exec-server 起不了自己的 Seatbelt（`sandbox_apply: Operation not permitted`，命令 rc 71）。所以实验里 exec-server 都起在沙箱外，传输程序去连它——产品里 exec-server 本来就在另一台机器上，不受影响。

## 3. 因此 P23 做成什么样

```text
Agent Node                                                         Runtime Node
Codex ──(它自己 spawn 的子进程，stdio)──> ccnm internal exec-transport ──exec──> /usr/bin/ssh <runtime> ccnm internal exec-serve --payload <protocol 6>
        CODEX_HOME=<session>/codex-home           读 session.json，exec 成 ssh                      P22 那一半：解析、审计、写锁、规则表、exec-server
```

| 件 | 在哪 | 做什么 |
| --- | --- | --- |
| `ccnm internal exec-transport` | `session/transport.rs`、`main.rs` | 载荷和 `agent-transport` 同一个（session 目录 + 身份），但另起一个动词：走错动词的构建在名字上就失败。读 `session.json`，要求它是链上的会话，exec 成 `ssh` 到 Runtime 的 `exec-serve`，ssh 选项与 MCP 传输逐项相同（`Ssh::exec_transport_cmd`） |
| 每会话 `codex-home/` | `provider/codex/native.rs` | supervisor 启动 Codex 前生成：`environments.toml`（`default = "ccnm"`、`include_local = false`、`program` 指向本机 ccnm）、`auth.json` symlink → profile 的 `auth.json`、`config.toml` 里一条对 Runtime 根的 `trust_level = "trusted"`。幂等；profile 校验照旧、不写 |
| Codex 原生启动参数 | `provider/codex/mod.rs` 的 `build_native_launch_cmd` | `--no-alt-screen`、`--sandbox workspace-write`、`-C <canonical 根>`、`approval_policy="on-request"`、`web_search="disabled"`、`agents.enabled=false`，`--disable` 关掉除 `shell_tool` / `unified_exec` / `unified_exec_tty` 之外的 DISABLED 列表（P21.2 实测的组合）；不注入 `mcp_servers.ccnm.*`；不传 Code Mode 排除名单——那个名单在 MCP 模式是用来把 Codex 自带的 `apply_patch` 藏起来的，这里恰恰要它。`CODEX_HOME` 指向会话的 `codex-home/` |
| opt-in 怎么到 Agent | `runtime.rs` 的 `ResolveReport.codex_exec_server`，`protocol/run.rs` 的 `StartRequest` / `RunRequest`，`session.rs` 的 `Spec` | 还是 Runtime 上那一行 `codex_exec_server = true`；`runtime-resolve` 报告带上它，并把 `root` 改报 canonical 路径（Codex 用 `-C` 拼出的每条 URI 都按它来，规则表按 canonical 判；root 不在 Runtime 上时 resolve 就报 `CCNM_E_WRONG_WORKSPACE`）。三个字段都只在为真时序列化，旧构建读别的请求不受影响。只对 Codex 生效：`Spec.codex_exec_server = 请求里的值 && provider == Codex` |
| print 拒绝 | `work.rs` 的 `run` | 选好 Agent 之后、拨 Runtime 之前，`CCNM_E_INVALID_ARGS`，什么都没创建；Claude 请求上的同一字段无事发生 |
| 预检 | `work.rs` 的 `native_runtime_preflight`，`Ssh::exec_transport_preflight` | MCP 预检之后再跑一次空会话的 `exec-serve`（stdin 立即关）：Runtime 没 opt-in、没 `codex_bin`、版本不对、exec-server 起不来，都在起 Codex 前报出来；退出码 0 但 stderr 首行是 `CCNM_E_*`（Tailscale SSH 不透传退出码）也按失败算 |
| 状态 | `session::transport::command` | 链上的会话回答的是 `exec-serve` 那条 ssh，所以 `ccnm status` 的 `TOOLS DOWN` 判断对它同样成立 |

**没做的**：不加任何依赖（WebSocket、SHA-1 都不需要）；不改 P22 的规则表；不给 Claude 开这条链；不动 profile 目录。`internal agent-transport` 拒绝链上的会话，`internal exec-transport` 拒绝链外的会话，两个方向都有测试。

## 4. 端到端实测（P23.4）

脚本、逐场景的原始结果和复跑方法在 toexec [`evidence/v2-c/p23-stdio/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p23-stdio/README.md)。拓扑：真实 Codex TUI（禁出站沙箱、临时 HOME、每会话 CODEX_HOME）→ `environments.toml` 指定的 `program` → 真实 `ccnm internal exec-serve`（P22 那一半，规则表、写锁都在）→ 真实 `codex exec-server`。**ssh 那一跳用本机 TCP 管道代替**：没有第二台 Runtime，给自己账号装 sshd 公钥是系统变更；而且 exec-server 起在 Codex 的 Seatbelt 里装不上第二层沙箱（spike 里命令全 rc 71），Runtime 半边必须在沙箱外。其余都是产品路径——`environments.toml` 的形状由本仓库的单元测试钉住，Codex 启动参数就是 `build_native_launch_cmd` 那张表。四个场景各跑 3 轮，`compare.py` 逐项一致：

| 场景 | 结果 |
| --- | --- |
| `basic`：读 `AGENTS.md`、写文件、`apply_patch`、再跑一条命令 | 传输程序 spawn 1 次、Runtime 1 条连接、`exec-serve` 退出 0、写锁 `released`；三个文件都在工作区，工作区外和 Agent 本机目录为空；模型拿到 Runtime 那份 `AGENTS.md`；会话中采样 Codex 进程树，**没有监听端口**；没有信任提示 |
| `refused`：模型申请提权命令写工作区外，再 patch 工作区外，人都批准 | 7 条 `-32600`：提权命令（sandbox null）1 条，越界 patch 3 条，Codex 自动 null 重试的 3 条；工作区外零文件；模型看到 `exec-server rejected request (-32600): ccnm refused process/start: a sandbox is required`；会话没死，下一条命令照常在工作区里执行 |
| `drop`：第一条命令结束时传输程序自杀 | Codex 不再 spawn 第二个（1 次）；第二条命令报 `exec-server transport disconnected`，哪里都没执行；`exec-serve` 见 EOF 退出 0、写锁 `released` |
| `unreachable`：`program` 是真的 `ccnm internal exec-transport`，Runtime 别名解析不了 | 传输程序 spawn 1 次，stderr 是 ssh 的 `Could not resolve hostname`；Codex 起来了但工具表是空的（模型报 `tools.exec_command is not a function`），Runtime 0 连接，没有任何文件——**没有退回本机执行** |

## 5. 门禁

macOS 26.6.2 arm64，rustc 1.98.0：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 通过；`cargo test --workspace` **759 passed / 0 failed**（P22 时 751，净增 8：`runtime` 1、`session::transport` 2、`provider::codex` 2、`work` 1、CLI 集成 `exec_transport` 2）；Python 168 passed；`check_protocol`、`check_plan`、`git diff --check` 通过。

## 6. 没做到、没测到的

- **ssh 那一跳的成功方向没有跑过**：`exec-transport` → `/usr/bin/ssh` → 远端 `exec-serve` 只验了失败方向（`unreachable`）；ssh 命令行本身由单元测试钉住，与 MCP 传输逐项相同。成功方向要两台机器，属 P24。
- 两台机器上装着的 ccnm 都还没有这条链；替换二进制、在 Runtime 装 Codex、跑真实模型回合都要逐项授权（P24）。
- Runtime 只在 macOS 上接过真 exec-server；Linux 沿用 P21 容器里的沙箱前提。
- auth.json symlink 的写穿透只用 `codex login --with-api-key` 验过，真实 ChatGPT 登录的 token 刷新走同一个 `save` 但没跑过。
- 没有并发会话争锁（P22 用假执行端测过互斥，本轮没重复）；没有 Codex 主动 `/exit` 之外的退出路径；没有真实模型。
- 断线时 Codex 手里有没写完的 patch 会弹"retry without sandbox?"，只在开发中看到一次，没进 3 轮。
