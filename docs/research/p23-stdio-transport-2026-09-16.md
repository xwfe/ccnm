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

（实现与第二批端到端实测见后文，随实现补写。）
