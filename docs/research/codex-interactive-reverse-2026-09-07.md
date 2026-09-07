# Codex interactive：本机 Agent → 远端 Runtime

**结论：这一步的真实 CLI / tmux / SSH stdio MCP / 凭据传输隔离测量已跑通。Codex provider 仍未开放，尚未接入产品 Controller/session dispatch。**

本轮只增加测量 helper、fixture 和回归，不更改 Claude 默认入口、现有配置、Runtime/MCP 实现或两端 ccnm 安装。

## 拓扑与登录

用户明确将本机作为 Agent Node、`fodelf` OpenSSH alias 对应的机器作为 Runtime Node。不在远端安装 Codex，也不绑定 alias 背后的具体网络产品。

- 本机：官方 Codex CLI `0.153.4`，用户随后安装 tmux `3.7c`；初始进程处于 `Aqua` 登录会话。
- 用户在本机桌面 Terminal 对 `~/.config/ccnm/agents/codex/` 独立执行官方 `codex login`。官方 `login status` 确认 ChatGPT 登录；目录 `0700`、认证文件 `0600` 且非符号链接。**没有读取、复制或链接默认 Codex 凭据。**
- 专用 HOME 的官方 `codex mcp list --json` 初始为空，没有个人 MCP 配置。
- 远端保留原有 ccnm `0.2.0`；它还是旧 `work/home` CLI。通过该二进制的 `init --help` / `workspace add --help` 确认参数后，只在新建临时目录生成 scratch 配置。
- 上一轮在远端创建的空 Codex HOME 已用 `rmdir` 精确撤销。非空目录会拒绝删除；没有清理任何登录状态。

专用 HOME 只由 Agent 端官方 CLI 使用，不出现在 Runtime 请求、MCP payload、Runtime 配置或 SSH 转发环境中。

## 已完成的四类测量

### 1. 真实 interactive，不是 exec 冒充交互

先在真实 PTY 中运行官方 Codex TUI，使用专用 HOME 和已测的工具限制。只确认了自己刚创建的临时 Agent cwd 的 trust 对话框，没有信任用户项目或放宽 sandbox。

模型通过 ccnm MCP 完整执行：

`workspace_info → list_files → search_text → read_file → apply_patch → exec_command → read_output`

远端文件变为 `CCNM_INTERACTIVE_RUNTIME_7319`；本机同名文件始终为 `WRONG_LOCAL_AGENT_9520`。路径、文件 version 和命令工作目录均来自 Runtime。

临时 PTY 驱动在 EOF 收尾时曾碰到 `killpg` 竞态，因此这次直接 PTY 的退出码**没有算作通过**。TUI 已显示 shutdown，进程确认退出；真正的退出码验证由下面的 tmux supervisor 完成。

### 2. tmux/session 持续性

使用全新、独立的 `tmux -L` socket 和 `-f /dev/null`，不碰用户默认 socket 或 ccnm 的现有 socket。Agent-local 测量 supervisor 在 pane 内启动 Codex，设置专用 HOME，并用临时文件 + rename 记录退出状态。

实测：

- `attach → detach → reattach` 的 attached 数为 `1 → 0 → 1`。
- tmux server、supervisor、Codex child、远端 MCP 的 PID 在这三步中全部不变。
- reattach 后继续同一个模型会话，按实际 version 把远端 marker 改成 `CCNM_TMUX_RUNTIME_7319`；本机文件仍不变。
- tmux 内 `launchctl managername` 为 `Background`，但官方 `login status` 仍成功。因此不把 `managername` 单独当作 Codex 登录是否可用的判据。
- 正常 `Ctrl-D` 后，supervisor 两次记录 exit 0，pane dead status 均为 0。

这里验证的是实际 tmux 和测量 supervisor 的生命周期，**不是尚未实现的 Codex → ccnm Controller auth/launch dispatch**。

### 3. MCP 断连与官方恢复

仅对这次 scratch 会话已经确认的远端 MCP PID 发 SIGTERM：

1. Codex 下一次 `workspace_info` 明确返回 `Transport closed`，没有改走本机工具。
2. 正常退出得到官方 session ID；不使用可能选错会话的 `--last`。
3. 根据当前 `codex resume --help`，用这个精确 ID、同一专用 HOME、同一工具策略恢复。
4. 新的远端 MCP PID 建立连接，真实读取到 `CCNM_TMUX_RUNTIME_7319`；原 tmux server 保持。
5. 最终退出后，所有本轮观察到的 MCP PID 均不再存在；仅清理本轮专用 tmux server。

这是**退出后官方 resume 恢复**，不声称存在自动 MCP 重连。当前 `/mcp` UI 曾显示 `connected (0 tools)`，但真实七工具调用正常；UI 计数不能替代真实 MCP 调用结果。

### 4. Credential isolation

`scripts/probe_codex_ssh_transport.py` 是本轮测量用的 stdio transport，不是产品 provider：

- Agent 侧启动 SSH **之前**剥离 `CODEX_*`、`OPENAI_*`、`CLAUDE_*`、`ANTHROPIC_*`。
- OpenSSH 使用已有 alias，指定 `SendEnv=-*`、`ForwardAgent=no`、`ControlMaster=no`、`ControlPath=none`。
- Runtime 用 `env -i` 构造固定、非敏感环境；其中 HOME/XDG/CCNM_CONFIG 全部是 **Runtime 自己的 scratch 路径**。
- 真实 `exec_command` 和 `read_output` 检查得到 `sensitive_environment_keys=[]`，含 SSH_AUTH_SOCK 检查；不读取环境变量的秘密值或任何 credential 文件。
- Codex 传给初版 MCP wrapper 的环境已经没有 CODEX_HOME；额外 wrapper 的删除保护仍保留。没有将“原本不存在”误写成“观察到了删除”。

本轮 Runtime 明确启用 `allow_unconfined_exec`，仅针对 scratch 项目。**这不是生产 ccrun、ACL、sudo、网络 egress 合规验收，也不意味着命令 parser 是 sandbox。**

## 重放要点

### 前置条件

在 Agent Node 桌面 Terminal 独立登录；不要通过 Runtime 传送 token：

```bash
CODEX_HOME="$HOME/.config/ccnm/agents/codex" /opt/homebrew/bin/codex login
CODEX_HOME="$HOME/.config/ccnm/agents/codex" /opt/homebrew/bin/codex login status
```

工具版本和二进制指纹见 `tests/fixtures/codex-0.153.4/reverse-interactive/manifest.json`。新版本必须重新测量，不能套用本轮 flags。

### Runtime scratch

只在 Runtime 新建独立临时 project/home/config。使用那台机器实际 `--help` 支持的命令生成配置；本次旧版 ccnm 使用 `init --work fixture-agent --home fixture-runtime` 和 `workspace add fixture <project> --allow-unconfined-exec`，均明确指定 scratch `--config`。不要把这些旧 flag 用在当前源码的新 Node CLI 上。

### Agent 端 MCP transport

在本轮 Codex 会话级 `-c` 配置中设置：

```text
command = Agent 本机的 Python 绝对路径
args = [
  Agent 本机 scripts/probe_codex_ssh_transport.py 的绝对路径,
  Runtime SSH alias,
  Runtime ccnm 二进制绝对路径,
  Runtime scratch project 绝对路径,
  Runtime scratch config 绝对路径,
  Runtime scratch home 绝对路径,
  本轮 session 标识
]
```

这六个业务参数全是 Runtime 路径或标识，没有 Agent CODEX_HOME 参数。stdin/stdout 直接保持 MCP 字节流，不包进另一个协议。

精确的已测 Codex resume argv 保存在 `reverse-interactive/resume-argv.json`（路径及 session ID 已脱敏）；首次启动去掉 `resume` 和 session ID，保留相同隔离策略。新建唯一 tmux socket，记录 PID 后做 attach/detach/reattach；结束用 Ctrl-D 并检查 supervisor exit 和 MCP 进程退出。不要把模型自报完成代替这些 OS/Runtime 观测。

### 离线回归

```bash
cargo test -p ccnm-core --test codex_measurements
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*codex*.py' -v
```

fixture 仅保留脱敏的登录状态、tmux 观察、关键屏幕片段、Runtime 环境结果和退出记录，没有认证文件。专用登录目录保留；scratch 目录保留在两端以便继续复测，生产配置未修改。

## 下一步，不提前开放

将这些实测契约接到内部 provider / Controller / session：Codex HOME 必须在 Agent 端解析，不能沿用 Runtime 发来的 Claude config-dir 字段；结果解析必须识别 JSONL 终态，缺失费用不能冒充 0；项目上下文还需验证远端 AGENTS 规则投射。完成接线和回归前，默认仍只有 Claude。
