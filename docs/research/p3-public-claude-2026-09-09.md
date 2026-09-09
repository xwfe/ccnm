# P3 Claude 反向公共链路实测

角色与 Codex 那轮相反：Agent 在 fodelf（官方 Claude 2.1.265），Runtime 是本机独立身份 `ccrun`（UID/GID 550 之外的既有账号，本轮已脱离 staff）。本机 bing 从 Runtime Node 发起公共 CLI。

## 部署与配置

当前 build（`8bf2efe21ec4281d1ff0b95b00cee64625cb5e351c28280a4489fdd372462bcc`）部署到两端，两侧各自 `shasum` 复核一致，不覆盖任何已安装 ccnm：

- Runtime：`/Users/Shared/ccnm-p3-runtime-local.5EL6Kk/`（bing 所有 0755，放 `ccnm-bin`、`config.toml`、wrapper `ccnm-runtime`）。ccrun 只读执行，自建私有 workspace `/Users/Shared/ccnm-p3-rtwork.WE8zVn`（0711，内部 project/state 归 ccrun）。整机传输走同机 `cp`，无网络环节。
- Agent：fodelf `/tmp/ccnm-p3-agent-claude.tw7Z1m/`（0700，含 `ccnm-bin`、wrapper `ccnm-agent`、独立 config/state/tmux 与 SSH 配置私有备份）。scp 9,732,640 bytes 用时 55.8 秒，远端哈希一致；上一轮的分块脚本本轮未使用。
- fodelf 新增唯一 SSH alias `ccnm-p3-local-5el6kk` → `ccrun@xdwmbp`，指定本轮 key、`IdentitiesOnly`、禁 agent 转发，前置写入以免被既有 `Host *` 抢先；原 alias 与备份保留。

wrapper 必须固定 `CCNM_CONFIG`/`XDG_STATE_HOME`/`TMUX_TMPDIR`。本轮先把二进制直接命名成 `ccnm-agent`，Agent 于是去读它自己的默认配置，`internal probe` 退回 protocol 1 且不返回 identity，公共入口报 `CCNM_E_VERSION: Agent response identity differs`。这是配置错误不是协议缺陷。

## 已验证

1. `mcp probe p3claude --agent claude-main --calls 3`：7 工具，初始化 612ms，3 次调用同一 server PID 44786，`single_process=true`。Runtime hello 返回 `user=ccrun` 与真实 root。
2. 公共 `run --print` 读文件：官方 Claude 退出 0，10.6s，2 turns，返回 `P3_PUBLIC_CLAUDE_OK`。`session.json` 为 protocol 3，`agent_identity` 完整（node/instance/provider/profile_ref），`claude_config_dir` 为 null——profile 路径没有进入 Runtime payload。
3. 工具真实执行的不可伪造证据：Claude 通过 MCP 编译并运行 C 程序，Runtime 侧复核 `hello` 是 Mach-O 64-bit arm64、`result.txt` 内容 `P3_CLAUDE_BUILD_OK`，两者 **owner 均为 ccrun**，重新执行输出一致。Claude 的 `--print` 不写官方 JSONL（项目历史目录只有空 memory），因此改用 Runtime 侧副作用取证，不以模型自述替代工具记录。
4. 精确 session：`status`/`result` 按 ccnm session id 正确返回，`Completed` 与 print 结果一致；伪造 session id 被明确拒绝。`stop` 后 session 记 `Failed`（受控终止），复查无残留 supervise/transport/tmux 进程。
5. Claude 登录在全程后仍有效（独立 print 返回 `LOGIN_STILL_OK` 并正常计费）。

## interactive 被官方 CLI 首次运行向导阻塞

`run --detached` 能起 tmux 与 supervise，session 记 `Running`，但 `status` 报 `TOOLS DOWN`。抓 tmux 窗格看到官方 CLI 停在 **主题选择向导**，Claude 尚未进入正常界面，所以 MCP 未连接。按回车后进入 **登录方式选择**；本轮没有继续按键，避免误触发 OAuth 或改变用户登录状态，随后正规 stop。

原因在官方 CLI 一侧：fodelf 的 `~/.claude.json` 有 `hasCompletedOnboarding`、`oauthAccount`、`userID`，但**没有 `theme` 键**——该用户平时用 Claude 桌面应用，CLI interactive 从未跑过，2.1.265 的主题步骤不在旧 onboarding 标记内。向导必须完整走完才落盘，中途 stop 后 theme 仍未保存，再起一次仍停在同一界面。print 模式无 UI，所以不受影响。

`claude in Background, keychain reachable` 不是故障：`session.rs` 已写明 tmux 必然 daemonize 出 `gui/` 域而报 `Background`，真正决定 Keychain 访问的是随之保留的 audit session，仅凭 `managername` 判断会误报。

期间观察到官方 CLI 自行做了配置迁移：新建 `~/.claude/.claude.json` 与 `backups/`，`~/.claude/.credentials.json` 消失。经独立 print 复核登录完好，判断是 2.1.265 把凭据迁往 Keychain，不是数据丢失。ccnm 未读取、复制或删除任何凭据。

另修改一处 Agent 侧个人配置：`~/.claude` 由 0755 收紧为 **0700**。这是 P1 profile 目录的硬性前置，不改则安全检查拒绝、Agent 不返回 identity；方向是收紧权限，回滚为 `chmod 755 ~/.claude`。

## 未完成与接续

interactive、detach/reattach、Ctrl-D、Controller 重启与 transport 故障均未取得证据，P3.1/P3.2 不能标记完成。下一步需用户在 fodelf 图形终端手动跑一次 `claude` 走完首次向导（主题、登录确认），使 theme 落盘；之后重跑 interactive 全套。这属于官方 CLI 的用户首次设置，不由 ccnm 代改配置文件。

本轮 Controller 由用户在 fodelf 图形终端前台启动（Aqua，PID 22991），未安装 LaunchAgent，未触碰既有的 `dev.ccnm.work-controller`。曾尝试用独立 label 临时 bootstrap，被权限策略拒绝，未绕过。

保留待清理资源：两端部署目录、fodelf SSH alias 与备份、Runtime workspace 与测试产物、本机组/准入变更。无 Rust 改动，不重报历史 Rust 数字。
