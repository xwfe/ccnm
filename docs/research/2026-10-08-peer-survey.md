# 业界和社区同类方案：ccnm 能借什么（2026-10-08）

**为什么有这份文件**：2026-10-08 看了 [marcfargas/tramp](https://github.com/marcfargas/tramp)（Go 写的 MCP，直接用操作者的 SSH 身份连远端），结论是不成熟，于是顺着查了二十多个同类项目，挑出对 ccnm 有用的做法。这里**只是调研**：不替代 `docs/plan/status.json`，也不自动认领新阶段；要做哪条，按 [ROADMAP](../plan/ROADMAP.md) 另立阶段。

**读法**：

- 每条都标了核实程度。"读源码"/"官方文档"表示有人逐行或逐段对过，链接是带 commit 的 permalink；"推断"是对照 ccnm 源码得出的判断，没有实测。**这份文件里没有一条做过真机实验**，P80 那条除外（它有自己的零额度实测记录）。
- Windows 相关的做法记在 [Windows 坑文档](2026-10-07-windows-openssh-pitfalls.md)第 5 节，这里不重复。

## 1. Claude Code 自己的多主机现状

### 1.1 已公开的三条路

| 功能 | 做什么 | 谁能用 |
| --- | --- | --- |
| [Remote Control](https://code.claude.com/docs/en/remote-control) | 本机跑着的 Claude Code，用手机或浏览器接着操作 | Pro、Max 也能用 |
| [云端会话](https://code.claude.com/docs/en/claude-code-on-the-web) | 会话跑在 Anthropic 的 VM 里，`claude --cloud` 发过去、`--teleport` 拉回来 | 需要 GitHub 仓库 |
| [Self-hosted environments](https://code.claude.com/docs/en/self-hosted-environments) | 云端会话改由你自己网络里的 runner 执行：runner 只往外发 HTTPS 轮询 Anthropic，领到会话就从 GitHub 克隆仓库，再起一个子 Claude Code 进程跑它；runner 停止轮询约 60 秒后租约失效，会话被重新排队；一个 runner 只服务一个 owner。命令行用 `--environment ccpool_...` 指定 | 公开 beta，只限 Team/Enterprise，默认关，要组织 Owner 在后台打开 |

### 1.2 打包代码里有、还没公开的："device tools"

在本机 Claude Code 2.1.286 的二进制里（`strings` 提取，2026-10-08）能看到一整套"云端会话把工具调用转发到你电脑上执行"的实现。截至 CHANGELOG 2.1.293、官方文档索引都没提到它：

- **怎么连**：你在自己电脑上用 `claude --cloud` 连上某个云端会话，这台电脑就登记成这个会话的"bound device"。会话里的 Read、Edit 等工具多了一个 `_host` 参数，用来选在哪台机器上执行；报错文案写的是 `Read it again there (Read with "_host": "…")`。
- **设备端先自检，不满足就整体拒绝服务**：
  - Claude Code 自己的沙箱（[sandbox-runtime](https://github.com/anthropics/sandbox-runtime)）必须开着；
  - 沙箱不能放开 Unix socket（`allowUnixSockets` / `allowAllUnixSockets`），也不能用弱化的网络隔离；
  - Linux 上要找得到 seccomp 辅助程序。

  以上任何一项不满足，这台设备就"serves no device tools"。
- **其他门槛**：
  - 执行前先问你"信不信这个会话挂着的仓库"，因为答"是"就等于让会话以你的身份操作你的电脑；
  - 组织可以要求可信设备认证；
  - 每条设备连接有并发上限；
  - 哪些目录对外提供，由同步设置决定。
- **平台和开关**：
  - 文案里有 `tool host helper: not available on Windows`；
  - 由账号级开关和云环境开关控制，对应的内部标志有 `tengu_remote_tool_forward`、环境变量 `CLAUDE_CODE_REMOTE_TOOLS_FORWARD`。

**对 ccnm 意味着什么（推断）**：

- 方向和 ccnm 相反。它是"模型在云上、工具在你电脑上"，走 Anthropic 的控制面；ccnm 是"模型在本机、工具在你的服务器上"，走你自己的 SSH。
- 它只服务 Claude 账号，不覆盖 Codex、ChatGPT；也不处理"执行端用一个独立账号"这件事。
- 要盯的信号：哪天它对 Pro/Max 开放、并允许一台 Linux 服务器登记成 device，"本机 Claude 用远端机器的工具"就有官方路线了。到那时 ccnm 还剩的价值就是：多客户端、不依赖 Anthropic 控制面、执行策略在 Runtime 一侧。
- **可以照抄的写法**：给模型的失败文案每条都说清三件事：这次到底跑没跑、能不能重试、该怎么跟用户说。例如 `The user's computer could not be reached right now; the call did not run. Do not call it again in this turn … tell the user what is blocked.` ccnm 的 `CCNM_E_*` 说明可以对照着补"别重试/可以重试"这一句。

**怎么盯**：每次升级 Claude Code 后跑一下。数字从 0 变成非 0，或者文案有变化，就重读这一节：

```bash
strings -n 6 "$(readlink -f "$(which claude)")" | grep -c 'device tools'
```

2.1.286 上输出 13（按行计数）。另外搜一下 [CHANGELOG](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md) 里的 `device`、`--cloud`。

## 2. 可借鉴清单（按对 ccnm 的用处排序）

| # | 做法 | 出处与核实 | 对 ccnm |
| --- | --- | --- | --- |
| 1 | Codex 调 MCP 工具默认 300 秒就放弃 | Codex 0.154.0 `rmcp_client.rs:103`（读源码） | **P80 已修**：受管会话传 `tool_timeout_sec=1870`；外部 bridge 的 Codex 用户要自己配。见 [P80 记录](2026-10-08-p80-codex-tool-timeout.md) |
| 2 | 长调用发 progress 通知 | Claude Code [MCP 文档](https://code.claude.com/docs/en/mcp)：空闲窗口内既没有响应也没有 progress 就中止；它自己的 `claude mcp serve` 每 30 秒发一次（CHANGELOG 2.1.271）；主会话里超过 2 分钟的 MCP 调用自动转后台（2.1.212）。Codex 只写日志（子代理读源码，未复核） | ccnm 一条都不发（搜过代码）。请求带 `progressToken` 时每 15–30 秒发一条"已跑 N 秒 · 最后一行"，Claude Code 上能显示进度、也不会被空闲超时掐掉 |
| 3 | 会"以人的身份生效"的文件默认拒写 | srt 的 [mandatory deny 清单](https://github.com/anthropics/sandbox-runtime/blob/3f0bad73/README.md#L871-L885)（官方文档）：文件 `.bashrc .bash_profile .zshrc .zprofile .profile .gitconfig .gitmodules .ripgreprc .mcp.json`，目录 `.vscode/ .idea/ .claude/commands/ .claude/agents/ .git/hooks/ .git/config` | [path.rs:179](../../crates/ccnm-core/src/mcp/path.rs) 只拒 `.git`。专用账号模式下，ccrun 写进 `.vscode/tasks.json` 或 `.mcp.json` 的东西，会在人用 IDE 或本机 Claude Code 打开这棵树时按人的权限执行，等于绕过 ccrun。共用账号模式下模型本来就能以你的身份跑命令，这条不额外提权 |
| 4 | 判断"文件变没变"用内容哈希 | Claude Code 2.1.286 打包代码（读源码）：读过整文件就记 `Bun.hash`（去掉 BOM、统一换行后算），之后比哈希，不比 mtime | ccnm 的 version 是 `大小-mtime`（[read.rs:201](../../crates/ccnm-core/src/mcp/read.rs)）。`touch` 或格式化后内容没变，apply_patch 也会拒；同样大小、又落在 mtime 精度内的改写会漏掉。代价是每次 patch 要把整个文件哈希一遍 |
| 5 | server instructions 前 512 字符要能单独读懂 | [Codex MCP 文档](https://developers.openai.com/codex/mcp)（官方文档） | 把 version 必传、output_ref 怎么用、没有 PTY 挪到 instructions 最前面 |
| 6 | 远端程序按版本放进各自的目录，缺版本就顺着同一条 ssh 推过去 | [Mutagen dial.go](https://github.com/mutagen-io/mutagen/blob/6ccfeaaf/pkg/agent/dial.go#L61-L66)：先跑 `~/.mutagen/agents/<版本>/`，失败才装。[VS Code paths.rs](https://github.com/microsoft/vscode/blob/40a45df2/cli/src/tunnels/paths.rs#L118-L158)：目录叫 `<quality>-<commit>`，有 prune。[DevPod inject.sh](https://github.com/loft-sh/devpod/blob/5a0efcbf/pkg/inject/inject.sh#L14-L139)：版本比对在 `inject.go`，从 stdin 收二进制，推送失败改走下载。[open-remote-ssh](https://github.com/jeanp413/open-remote-ssh/blob/021ecb67/src/scripts/server-setup.sh#L83-L113)：安装前 `flock -x -w 30`（以上都是读源码） | ccnm 只有一个固定路径 `~/.local/bin/ccnm`（[config.rs:40](../../crates/ccnm-core/src/config.rs)）。按版本分目录后，旧会话接着跑旧文件，升级不用先停会话。前提：这个目录 ccrun 写不了（否则模型能替换执行策略本身）；要配 prune，才符合"不留旧版本" |
| 7 | 每次写之前用影子 git 打快照 | Gemini CLI [checkpointing](https://github.com/google-gemini/gemini-cli/blob/44d764ee/docs/cli/checkpointing.md#L8-L58)（官方文档，默认关）：快照放 `~/.gemini/history/<hash>`，用 `/restore` 回滚 | Claude Code 的 `/rewind` 只管它本机的文件，ccnm 改的是 Runtime 上的文件，一个都撤不回。可以让 mcp-serve 在 apply_patch/exec_command 之前，用 state 目录里一个独立的 `GIT_DIR` 打快照（遵守 .gitignore），回滚只给人用。写入互斥保证打快照时只有一个写者。大仓库要多久，得实测 |
| 8 | 收掉离开进程组的后代进程 | Linux `PR_SET_CHILD_SUBREAPER`（[prctl(2)](https://man7.org/linux/man-pages/man2/prctl.2.html)，让脱离出去的孤儿进程仍挂在 mcp-serve 名下）；macOS 照 Codex 的 [pid_tracker.rs](https://github.com/openai/codex/blob/82e70121/codex-rs/cli/src/debug_sandbox/pid_tracker.rs#L5-L6) 用 kqueue `NOTE_FORK` 加 `proc_listchildpids`（读源码） | 补 [P43](p43-guard-recovery-2026-09-20.md) 记下的漏洞：后代进程 setsid 出去以后，写锁照样交了出去。P43 当时把这类进程容器列为另立范围 |
| 9 | 等"就绪"，不 sleep 轮询 | Cloudflare Sandbox 的 [`waitForLog` / `waitForPort`](https://developers.cloudflare.com/sandbox/api/commands/#processwaitforport)（官方文档）：区分 `ProcessExitedBeforeReadyError` 和 `ProcessReadyTimeoutError` | 给 `read_output` 加一个 `until` 正则，匹配到就返回，并分清"进程先退出了"和"等超时了" |
| 10 | 被沙箱挡了什么，告诉模型 | srt 在输出后附 `sandbox_violations`（官方文档）；`codex sandbox macos --log-denials`（子代理读源码，0.154.0 有没有未核实） | 开了 `exec_sandbox = "codex"` 时，模型现在只看到一句 `Operation not permitted`；把被挡的项写进结果 notes |
| 11 | MCP 2026-07-28 版协议 | [规范 changelog](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/0a11bf68/docs/specification/2026-07-28/changelog.mdx#L14-L80)：无状态化，新增 `server/discover`；server 要输入改用 MRTR（先回 `input_required`，客户端带上答复重发原请求）；Tasks 移出核心协议成为扩展；Roots、Sampling、Logging 弃用。Claude Code 2.1.292 起对 stdio server 默认协商这一版，`MCP_PROTOCOL_NEGOTIATION=legacy` 可回退（[CHANGELOG](https://github.com/anthropics/claude-code/blob/79babc37/CHANGELOG.md)） | ccnm 用 rmcp 3.2.0，按[协议第 9 节](../protocol/remote-workspace-mcp-v1.md)会回 2025-11-25。**要实测一次**：新版 Claude Code 实际协商到哪一版，instructions 和取消是否照常送达。以后加任何 server 发给客户端的请求，两条路径都要写；不要接 roots |
| 12 | 跨文件替换：先预览、再按编号改 | Serena 的 [`replace_in_files`](https://github.com/oraios/serena/blob/3b99f8b0/src/serena/tools/file_tools.py#L163-L215)（读源码）：dry_run 返回带编号的最小 diff；编号有任何一个未知或过期就一处不改；也可以给 `expected_count`，数量不对就不改 | 优先级低。真要做跨文件改名，编号里带上文件 version，照样防得住"文件被改过" |
| 13 | 分清"装着的版本"和"正在跑的版本"，pid 带上开机 ID | Codex [app-server-daemon](https://github.com/openai/codex/blob/82e70121/codex-rs/app-server-daemon/README.md#L39-L74)：`daemon version` 同时报两者；[pid_identity.rs](https://github.com/openai/codex/blob/82e70121/codex-rs/app-server-daemon/src/backend/pid_identity.rs#L1-L40) 用 boot_id 加进程启动时刻，防 PID 被复用（读源码） | 写入 guard 被别的版本占着时，直接报"vX、pid N 还占着"。pid 身份这套做法 gld 也能用 |

**要你拍板的一条**：出口按域名放行、凭据在代理处注入。

- 做法：srt、Codex [network-proxy](https://github.com/openai/codex/blob/82e70121/codex-rs/network-proxy/README.md#L57-L77)、Vercel、Docker sbx 都是这一套。按域名放行写在 README 里；"代理处替换凭据"在 Codex 的 `credential_broker` 代码里，README 没写。
- 好处：能解决 [P33](p33-exec-sandbox-2026-09-17.md) 实测到的问题。codex 沙箱完全断网，cargo 下不了依赖。
- 冲突：和 [production-safety.md](../production-safety.md) 里"ccnm 不提供 egress isolation"相冲突；而且 srt 依赖 Node。

## 3. 看过、不借的

这一节只记结论。出处由调研子代理读过，除了标"读源码"的几处，都没有逐条复核。

| 做法 | 出处 | 为什么不借 |
| --- | --- | --- |
| 断线自动续连、应用层心跳 | Zed（5 秒一次心跳，丢 5 次就重连）、VS Code（3 小时重连宽限） | 撞"不做 resume"；Codex [#41573](https://github.com/openai/codex/issues/41573) 还说明，同一条 stdio 上加心跳会被大消息堵住 |
| 常驻 PTY shell、往进程 stdin 写 | OpenHands、SWE-ReX、Codex unified_exec、desktop-commander | 撞"不做 PTY" |
| 按命令文本分类、白名单 | Codex execpolicy、各种 ssh-mcp | 撞"不猜命令是否只读" |
| 预览 URL、端口转发 | E2B、Daytona、Modal、Sprites、tramp | 撞"不做通用端口转发" |
| MCP Tasks 扩展 | 规范 2026-07-28 | Claude Code 2.1.286 会把工具上的 `execution` 字段剥掉；ccnm 现有的 output_ref 已经够用（子代理读打包代码，未复核） |
| 用 elicitation 当审批闸门 | tufantunc/ssh-mcp | Codex 在审批策略为 never 且全盘可写时，会自动接受不带字段的确认（[elicitation.rs](https://github.com/openai/codex/blob/82e70121/codex-rs/codex-mcp/src/elicitation.rs#L577-L620)，读源码）。elicitation 只适合必须由人回答、表单非空的场景 |
| 远端零安装，直接 SSH/SFTP | tramp | ccnm 的写入互斥、.gitignore 过滤、整组杀进程超时都要在 Runtime 一侧做，零安装就等于放弃这些 |

给 gld 的一条：ChatGPT Apps SDK 要求三项 annotations（readOnly、destructive、openWorld）都写，并且支持 `_meta["openai/toolInvocation/invoking"]` 和 `invoked`（各不超过 64 字符的状态文字），见 [Apps SDK 参考](https://developers.openai.com/apps-sdk/reference)（官方文档）。
