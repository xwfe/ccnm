# P74 Linux 当 Agent Node（2026-10-07）

**为什么做**：用户指出 README 写"跑 AI 的机器只能 macOS"会让人以为项目很局限。核对代码后发现这是"没做"而不是"做不了"：拉起 AI 的 Controller 只有 launchd 版，起会话前要求 `launchctl managername` 是 `Aqua`。在 hpsrv（Debian 13）上用 v0.11.2 看到的样子：

```text
$ ccnm controller install --dry-run
write   /home/ccrun/Library/LaunchAgents/dev.ccnm.controller.plist
run     /bin/launchctl bootout gui/1002/dev.ccnm.controller
run     /bin/launchctl bootstrap gui/1002 /home/ccrun/Library/LaunchAgents/dev.ccnm.controller.plist
```

用户定：做，目标是三端（macOS、Linux、Windows）都支持；Windows 另立设计，不在本阶段。

**证据范围**：离线测试与本机门禁。**真机还没验**（第 5 节），没有发版。

## 1. 设计

**那道 `Aqua` 检查管的是什么。** macOS 上 Claude Code 的登录在登录钥匙串里，只有图形登录会话读得到；从 ssh 起的进程会以为自己没登录（`controller.rs` 开头那张 2026-09-03 的实测表）。所以 Controller 必须由 launchd 起在 `gui/<uid>` 里，起会话前也要核对。Linux 上没有这回事：Claude Code 的登录是 `~/.claude/.credentials.json`，Codex 是 `auth.json`（ccnm 的凭据路径表里本来就这么列），这个账号的任何进程都读得到。Linux 上 Controller 仍然要有——会话要活过发起它的那条 ssh——但"是不是图形会话"不是该问的问题。

| 问题 | macOS | Linux（P74） |
| --- | --- | --- |
| Controller 由谁托管 | launchd LaunchAgent（`~/Library/LaunchAgents/dev.ccnm.controller.plist`） | systemd 用户服务（`$XDG_CONFIG_HOME/systemd/user/dev.ccnm.controller.service`） |
| 单元里写什么 | `CCNM_CONFIG` / `XDG_*`，不写 `PATH` | 同左；另有 `Restart=always`、`RestartSec=10`、`KillMode=process`、`StandardError=append:<controller.log>` |
| 起会话前的判断 | `managername` 必须是 `Aqua` | Controller 回话即可，登录由 CLI 的 `auth status` 证明 |
| 退出登录 | 锁屏、退出都不影响已装的 LaunchAgent | 默认停掉用户实例（连同 Controller 与会话）；要常驻得开 linger，ccnm 不替你开 |

几个取舍：

- **平台由 Controller 自报**（会话信息新加可选字段 `platform`、`linger`）。`doctor` 常在另一台机器上读它——macOS 的 Runtime 读 Linux 的 Agent——按读的那台机器判就错了。没有 `platform` 的是 P74 之前的 Controller，那些都在 macOS 上，按 macOS 判；未知的不放行。
- **`KillMode=process`**：`controller install` 在升级时会重启 Controller；systemd 默认会把同一个 cgroup 里的进程（Controller 起的 tmux、会话）一起杀掉，等于升级一次就断掉别人的对话。和 launchd 版"重装不影响已有会话"保持一致。
- **不写 `PATH`**：和 plist 同理。Controller 在服务的环境里找 `claude` / `codex`，找不到再看 `~/.local/bin`、`/usr/local/bin` 等常见位置，这就是会话实际拿到的答案。
- **linger 不替人开**：它改的是机器怎么对待这个账号、要管理员权限。没开时 `install`、`status` 和 doctor 的 Controller 行（注意）写清退出登录会发生什么、敲什么。
- **其他系统**（包括 Windows）：明确说"这个系统还没有 Controller"，不猜。

## 2. 实现

| 位置 | 改了什么 |
| --- | --- |
| `crates/ccnm-core/src/systemd.rs`（新） | 单元文件、安装计划（`daemon-reload`、`enable`、`restart`）、install / uninstall；路径里的 `%`、`"`、`\` 按 systemd 规则转义，带换行的路径、带空白的日志路径拒绝；没有用户实例（`su` 进来的）时报错说怎么办；linger 提示 |
| `controller.rs` | `Host`（macOS / Linux / 其他）；`Context::on(runner, host)` 按平台测；`Context` 加 `platform`、`linger`（空时不序列化，旧读者照常读）；`login_session()` 按 Controller 自报的平台判；`Tools.host`；等 Controller 起来的轮询挪到这里两边共用；"停/重启 Controller"的提示与"没在监听"的解释按平台说 |
| `session.rs` | 会话信息里的托管方式按平台测，钥匙串探测只在 macOS 做 |
| `doctor.rs` | Controller 行：linger 关时记注意，写明后果与命令 |
| `ccnm-cli/src/main.rs` | `controller install / status / uninstall` 在 Linux 走 systemd，其他系统明确拒绝；中英文帮助不再写死 LaunchAgent |
| `paths.rs` | `config_home()`（`$XDG_CONFIG_HOME` 或 `~/.config`） |

`ccnm.machine/1`、`ccnm.workspace-mcp/1` 和内部协议号都没变。

## 3. 测试

| 用例 | 证明什么 |
| --- | --- |
| `a_linux_controller_can_start_agents_without_a_gui_session`（`controller::tests`） | Linux Controller 报回的会话信息（JSON）在任何机器上读都算"能拉起 AI"。**旧代码上红**（只认 `Aqua`，判成读不到登录）——这条写成旧代码也能编译的样子，先在旧代码上跑过 |
| `a_linux_controller_measures_itself_without_launchctl` | Linux 上不调 launchctl，只调一次 `loginctl show-user … --property=Linger --value`；托管方式是两种说法之一；序列化出 `"platform":"linux"` |
| `a_context_without_a_platform_is_read_as_macos` | 旧 Controller（没有 `platform`）按 macOS 判，`Background` 不放行；macOS 的新会话信息里不出现 `linger` |
| `other_systems_say_there_is_no_controller_yet`、`linger_is_yes_no_or_cannot_say` | 其他系统不调任何命令、明确说没做；linger 只认 `yes` / `no` |
| `systemd::tests` 七条 | 单元内容（`ExecStart`、`KillMode=process`、`Restart`、日志、不写 `PATH`）、转义、拒绝写不进单元的路径、计划里列出每条命令、没有用户实例时的报错、uninstall 的顺序、linger 提示 |
| `a_linux_controller_without_linger_is_a_warning_that_names_the_fix`（`doctor::tests`） | linger 开着是正常，关着是注意并给出命令，说不清时不报 |

原有用例里给假 `launchctl` 喂输出的（`controller`、`work`、`launcher`、`rpc` 和 CLI 的 `instance_execution` 等 14 处）显式指定 macOS，在 Linux CI 上结果不变。

## 4. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载 22–39：fmt、clippy（1.98 与 `+1.99.0`）、`cargo +1.89 check --locked` 通过；`cargo test --workspace` 默认与 64 线程各 1092/0（P73 后 1079，新增 13 条）；`ci_gates.py` 通过（计划、协议、Python 262 条 0 跳过）。线上 Linux CI 等推送后看。

## 5. 还没做的

- **真机**：hpsrv 的 `bing` 现在当不了 Linux Agent——没开 linger、没有活着的用户实例，机器上也没装 tmux，`bing` 名下没有 Claude / Codex。要验得先另行授权：装 tmux、给一个账号开 linger（或临时起它的用户实例）。零额度能走到"Controller 由 systemd 起来、doctor 正常、起会话并精确停止、用假模型跑一次到 Runtime 的工具调用"；**用真实模型要用户自己在那个 Linux 账号上登录 Claude 或 Codex**（ccnm 不复制凭据）。
- **没发版**：v0.11.2 及之前的 Linux 包不能当 Agent。
- **Windows**：两边都没做，要先另立设计。
