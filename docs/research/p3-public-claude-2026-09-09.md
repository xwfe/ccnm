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

### 对照实验：controller 架构的正面证据

在 fodelf 的 SSH 会话里直接跑官方 `claude --print`（绕开 ccnm），返回 `Not logged in · Please run /login`；同一台机器、同一用户、同一份 `~/.claude`，经 ccnm 由 Aqua 里的 controller 启动就正常计费返回结果。

这证实凭据只在 login Keychain、只有登录会话可读，也正面验证了 controller 的存在价值：它不是多余的一层，去掉它 Agent 就会得到「未登录」这个假答案。设计文档里「ssh 会话问 Claude 登录状态必然得到假否定」的判断，本轮有了真机反例支撑。

据此可以确定 interactive 停在向导不是凭据不可达造成的——真读不到时官方 CLI 直接回 `Not logged in`，而不是渲染主题选择界面。缺的是 `theme` 未落盘导致向导每次重来。

期间观察到官方 CLI 自行做了配置迁移：新建 `~/.claude/.claude.json` 与 `backups/`，`~/.claude/.credentials.json` 消失。经独立 print 复核登录完好，判断是 2.1.265 把凭据迁往 Keychain，不是数据丢失。ccnm 未读取、复制或删除任何凭据。

另修改一处 Agent 侧个人配置：`~/.claude` 由 0755 收紧为 **0700**。这是 P1 profile 目录的硬性前置，不改则安全检查拒绝、Agent 不返回 identity；方向是收紧权限，回滚为 `chmod 755 ~/.claude`。

## 向导走完后 interactive 成立

关键对照实验推翻了「tmux 丢了 audit session」这一猜测：在 controller 启动的**同一个 tmux server 里**直接跑官方 `claude --print`，返回 `TMUX_AUTH_OK`。tmux daemonize 后 audit session 确实保留，Keychain 可读，凭据自始至终正常。所以 interactive 停在向导只是官方 CLI 的首次运行 UI 流程，与认证无关；先前据 print 行为推断 interactive 的路径不成立，以本实验为准。

用户在 fodelf 终端 `ccnm attach` 走完向导：官方 CLI 认出既有账号（`Login successful`），随后是安全提示与工作目录信任确认。信任确认前先记录 `~/.claude.json` 原有 53 个 project entry 且本轮路径不在其中，本轮新增的 entry 待验收后精确移除，不动其余条目；记录留在 Agent 目录 `trust-entry-path.txt`。

之后 Claude 进入正常界面（v2.1.267，Opus 5，Claude Max），`status` 转为 `tools connected`。interactive 下实测工具调用：官方 CLI 显示 `Called ccnm 2 times`，返回 `P3_INTERACTIVE_READY`，工具事件以官方 CLI 自身记录为准，不取模型自述。

`theme` 键在两个 `.claude.json` 中始终未出现，但向导已不再重复；本轮不追究官方 CLI 的存储位置，也不代改其配置。

## 生命周期与故障注入

6. detach：客户端全部分离后 `status` 仍为 `tools connected`，Agent 与 MCP 存活。
7. reattach：`ccnm attach` 经 PTY 重新连上（`/dev/ttys020`，xterm-256color），画面恢复到同一会话的既有输出。
8. Agent 自然退出：官方 CLI 对 `send-keys C-d` 无响应——输入框为空（`C-u` 后占位提示不变可证）、有无 attached 客户端都试过，均不退出，这是 Claude Code 2.1.267 自身行为，与 ccnm 无关，本轮据此**不能**宣称验过 Ctrl-D。改用 `/exit` 验证同一条「Agent 自己结束」路径：tmux server 消失，session 记 **Completed**（区别于 stop 的 Failed），Runtime 侧 mcp-serve PID 42437 消失，guard 由 `held 94818b0d… p3claude` 转为 `released`。
9. transport 故障注入：空闲 interactive 下定位 Claude 的子进程 SSH（PID 49781），先核对其父进程命令行含本 session id 才发 TERM，不对历史 PID 或同名进程动手。终止后 Claude 仍存活，`status` 正确区分 **Agent 仍在 detached 而 TOOLS DOWN**；Runtime 侧 mcp-serve 随之消失且 guard 正常 `released`。随后精确 `stop` 成功，session 记 **Failed**（受控终止）。这是 transport 进程故障，不等同物理断网。

`stop` 返回时 tmux 与 supervisor 已确认结束，但官方 CLI 进程尚存活约 10 秒后自行退出。契约要求确认的是 supervisor/tmux，故不算残留；记此一笔以免下次把异步退出误判为泄漏。

两端最终复核：Agent 侧无 tmux/supervise/transport 残留，Runtime 侧无 `ccnm-bin internal mcp-serve`，guard `released`，`status` 无存活 session。

## Controller 重启

10. Controller 缺失时的行为先被意外验证：用户关闭前台窗口后 controller 消失，`run` 明确报 `CCNM_E_NOT_READY: nothing is listening on …/controller.sock` 并解释「ssh 会话读不到 login Keychain，所以没有 controller 就无法核对 Claude 登录」。诊断准确可操作。但它附带的修复建议 `launchctl kickstart -k gui/$(id -u)/dev.ccnm.controller` 在本轮场景下不适用——该判断只看到 `~/Library/LaunchAgents` 里存在 ccnm 的 plist，而那是用户既有的 `dev.ccnm.work-controller`（旧二进制、默认 socket 路径），执行只会拉起原服务，不会监听本轮临时 socket。手动 controller 场景需要手动重启，本条建议不能照搬。
11. 重启本身：基线 controller PID 51395（Aqua）、tmux server 51458（PPID 1）、supervise 51459（父进程是 tmux 而非 controller）、`tools connected`。用户在图形终端 Ctrl-C 后重新启动，PID 变为 51607 且仍为 Aqua；既有 session `9adb492c` 保持 **Running 且 tools connected**，跨重启精确 `attach --session` 成功（客户端 `/dev/ttys017`，画面渲染出官方 UI）。supervise 挂在 tmux 而非 controller 之下，是 session 能扛过 controller 重启的结构原因。

随后精确 stop，两端复核无残留：Agent 侧无 tmux/supervise/claude，Runtime 侧无本轮 `mcp-serve`，guard `released`。

## 未完成与接续

P3.1/P3.2 的公共链路证据已齐：probe、print、工具真实性、精确 session、stop、interactive、detach/reattach、Agent 自然退出、transport 故障、Controller 重启均有真机记录。

两项限制如实保留：Ctrl-D 因官方 CLI 无响应未取得证据，仅以 `/exit` 覆盖同一条自然退出路径，不记作 Ctrl-D 通过；本轮是 transport 进程故障注入，不等同物理断网，egress/网络策略仍未逐项验证。

## 清理执行结果

本机已归零：ccrun 主组还原为 `gid=20(staff)`、本轮独立组删除、SSH 准入移除后 `com.apple.access_ssh` 恢复无直接成员且仅嵌套 admin、home 回到 `ccrun:staff` 0700。追加的公钥行按清单精确移除 183 字节，原有 96 字节保留（不盲目回滚整份 authorized_keys），root 清单 `/var/db/ccnm-p3-local-20260908` 删除。两端 SSH config 的本轮 alias 区块各自精确移除（本机 88→77 行、fodelf 43→32 行），其余 Host 条目完整。本机 SSH config 前置本轮 block 期间把 OrbStack 的说明行挤离首位，移除后已回到原位。

两端部署目录、Runtime workspace 与测试产物、两轮 `/Users/Shared/ccnm-p3-*`、本机一次性密钥与临时日志脚本、`~/.claude.json` 中本轮新增的 project entry（1→0，旧文件 53 条未动）均已清除。`~/.claude` 按用户要求恢复为 0755。

追加公钥的脚本原先只有 `--apply`，清理阶段才发现缺逆操作，已补 `p3-revoke-local-runtime-key.sh`。账号清理脚本同样原先不存在，补为 `p3-cleanup-runtime-user.sh`；其首版用了 `mapfile` 与关联数组，在 macOS 自带 bash 3.2 上直接 `command not found`——`bash -n` 查不出这类问题，已改写并新增静态检查拦截 bash 4+ 特性。

仍未归零：fodelf 临时账号 `ccnmp3test`、其独立组、home 与 root 清单 `/var/db/ccnm-p3-account-20260908`，以及存放清理脚本的 `/tmp/ccnm-p3-setup.TnaRle`。删除账号需要用户 sudo 执行 `p3-cleanup-runtime-user.sh --apply`；UID550 已无活动进程，前置条件满足。P3 在此归零前保持未完成。

本轮 Controller 由用户在 fodelf 图形终端前台启动（Aqua，PID 22991），未安装 LaunchAgent，未触碰既有的 `dev.ccnm.work-controller`。曾尝试用独立 label 临时 bootstrap，被权限策略拒绝，未绕过。

保留待清理资源：两端部署目录、fodelf SSH alias 与备份、Runtime workspace 与测试产物、本机组/准入变更。无 Rust 改动，不重报历史 Rust 数字。
