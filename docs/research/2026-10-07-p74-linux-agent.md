# P74 Linux 当 Agent Node（2026-10-07）

**为什么做**：用户指出 README 写"跑 AI 的机器只能 macOS"会让人以为项目很局限。核对代码后发现这是"没做"而不是"做不了"：拉起 AI 的 Controller 只有 launchd 版，起会话前要求 `launchctl managername` 是 `Aqua`。在 hpsrv（Debian 13）上用 v0.11.2 看到的样子：

```text
$ ccnm controller install --dry-run
write   /home/ccrun/Library/LaunchAgents/dev.ccnm.controller.plist
run     /bin/launchctl bootout gui/1002/dev.ccnm.controller
run     /bin/launchctl bootstrap gui/1002 /home/ccrun/Library/LaunchAgents/dev.ccnm.controller.plist
```

用户定：做，目标是三端（macOS、Linux、Windows）都支持；Windows 另立设计，不在本阶段。

**证据范围**：离线测试、本机与 Debian 13 上的全量测试、hpsrv 上的零额度真机（第 5 节：systemd 起 Controller、doctor、交互会话、到 Runtime 的工具调用、重启 Controller 不断会话、精确停止），以及同一拓扑上一次真实模型（第 6 节，Codex，经代理）。没有发版。

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

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载 22–39：fmt、clippy（1.98 与 `+1.99.0`）、`cargo +1.89 check --locked` 通过；`cargo test --workspace` 默认与 64 线程各 1092/0（P73 后 1079，新增 13 条）；`ci_gates.py` 通过（计划、协议、Python 262 条 0 跳过）。

**Linux 上漏改的两条旧断言**：推送后线上 Linux CI（run 37601989595）红在 `cli.rs` 的 `controller_install_carries_moved_locations_into_the_plist`——它跑真二进制的 `controller install --dry-run`、只认 plist，P74 之后 Linux 上打印的是 systemd 单元；随后在 hpsrv（Debian 13，rustc 1.98.1，以 ccrun 用同一份源码 `--offline`）跑全量时又红一条 `a_dead_controller_and_a_missing_one_get_different_advice`（只认 `kickstart`）。两条都是旧断言没按平台改、产品行为是对的，按平台断言后（`1632ead`、`49e7c70`）Debian 13 上 `cargo test --workspace --no-fail-fast` 1092/0。本机是 macOS，这两条在本机看不见——以后改按平台分支的代码，要在 Linux 上跑一遍再推。

## 5. 真机（零额度）

用户授权：hpsrv 装 tmux（3.5a）、给 `bing` 开 linger、在 hpsrv 上以 ccrun 现编候选并临时装给 bing（验完删掉）。

**拓扑**：Agent 是 hpsrv 的 `bing`（Linux，候选构建 `d7d24095…`，后换 `39616d7b…`），Operator 就在 Agent 上，Runtime 是同一台机器的 `ccrun`（v0.11.2 发布版，版本号与内部协议号和候选相同），bing 用一次性密钥经 `127.0.0.1` 连 ccrun（`authorized_keys` 加 `from="127.0.0.1,::1"`，主机指纹与 `/etc/ssh/ssh_host_ed25519_key.pub` 核对一致）。

**AI 用假的，不花额度**：Codex 0.154.0（Linux musl 版；GitHub release 包按 digest `d7e18b25…` 核过，包里二进制与 ccrun 已有那份 sha256 相同 `3188814c…`）放 bing 的 `~/.local/bin`；ccnm 专用 Codex profile 里用 `codex login --with-api-key` 登记一个明显是假的 key，`config.toml` 把模型服务指向本机 fake 模型（P71 夹具复制一份，调用改成数组）；实例写 `model = "gpt-5.1-codex"`，ccnm 不开 Code Mode，工具在顶层。fake 模型用 `systemd-run --user` 起，不和 ssh 绑在一起。

| 步骤 | 结果 |
| --- | --- |
| 从 `su` 进来、不带 `XDG_RUNTIME_DIR` 装 | 报 `Failed to connect to user scope bus … $XDG_RUNTIME_DIR not defined`，并给出办法。**真机查出两处措辞问题**：原提示说"没有用户实例，去开 linger"，而 linger 已开、实例在跑，照做解决不了（`90c82f7` 改成两种原因都写）；新措辞每行末尾拖着空格（`bfa42cc` 修，用例补"每行不以空格结尾"）。两处都只在离线证明了改后的样子 |
| 带上 `XDG_RUNTIME_DIR` 装 | 退出 0：`listening: ccnm 0.11.2 as bing, pid 2088514, systemd user service`；单元 enabled，`KillMode=process`、`Restart=always`、active；linger 开着，没有提示 |
| `ccnm doctor p74codex`（bing） | `可以用了（3 项不查……）`、退出 0；Controller `systemd user service`；Codex 0.154.0 是从 `~/.local/bin` 找到的（服务的 PATH 里没有）；`logged in via API key`；终端会话 tmux 3.5a；远端 MCP 握手 11 个工具 |
| `ccnm run p74codex --detached` | 会话信息写 `codex 在 systemd user service`；信任提示选 Yes 后，fake 模型的 `read_file README.md` 从 Runtime 读回内容；第一条 `exec_command` 弹出 Allow / Cancel（P71/P72 的审批在 Linux Agent 上一样），放行后 ccrun 写出 `p74-a.txt`（`P74-A-073ae3`） |
| 第二条提示还开着时 `ccnm controller install`（即升级时的重启） | Controller pid 2088514 → 2089448；tmux（2088968）、会话监督进程（2088969）、Codex（2088979）、Runtime 上 ccrun 的 `mcp-serve`（2089188）都还是原来的进程，`ccnm ls` 显示在跑、工具通 |
| 之后选 Cancel | 模型收到 `user cancelled MCP tool call` 并收尾；`p74-b.txt` 不存在，Runtime 上只有一条输出记录——会话在 Controller 重启后照常工作 |
| `ccnm stop p74codex --session …` | 退出 0；tmux 没了，没有会话进程残留，写锁 `released`，`ccnm log` 记"被停止" |

**收尾**：Controller 用 `ccnm controller uninstall` 卸掉（停用、删单元、`daemon-reload`），fake 模型服务停掉；bing 名下本轮建的（`~/.config`、`~/.local/bin`、`~/.local/state`、`~/p74`、Codex 自建的 `~/.codex`、`~/.ssh` 下的 config / 一次性密钥 / known_hosts）全删，家目录与开始前逐项一致；ccrun 的 `authorized_keys` 回到 0 字节，临时配置、测试仓库、构建目录、`codex --version` 时 Codex 自建的 `~/.codex` 删掉；`/tmp` 下两个 tmux 目录删掉。**保留** tmux 和 bing 的 linger（用户授权装的，真实模型那一步还要用）。

## 6. 真实模型

**第一次登录没成**，原因有两层，都不是 ccnm 的问题，但都值得写下来：

1. 用户第一次说"已登录"时，hpsrv 的 bing 名下既没有 `codex`，也没有任何 Codex 登录（全盘找不到 Codex 的 `auth.json`），登录发生在别处。ccnm 只读专用目录 `~/.config/ccnm/agents/codex`，所以给了一条带 `CODEX_HOME=` 的登录命令（Codex 0.154.0 有 `--device-auth`，不需要图形界面）。
2. 照这条命令登录，报 `device code request failed with status 403 Forbidden`。从 hpsrv 探测：OpenAI 登录接口回 `{"error":{"code":"unsupported_country_region_territory",...}}`，`api.openai.com`、`chatgpt.com` 连不上，出口国家是 CN。**跑 AI 的机器必须能访问 AI 服务**，模型的每一次调用都从这台机器发出去。

**怎么让 hpsrv 出去**（用户选的）：本机开一条 `ssh -R 127.0.0.1:17890:127.0.0.1:7890 hpsrv`，把本机代理借给 hpsrv，只绑回环地址，验完即断；经它访问 `api.openai.com` 回 401（通）。只给 bing 的 systemd 用户实例 `set-environment HTTPS_PROXY / HTTP_PROXY / NO_PROXY`，`ccnm controller install` 重启后，Controller 进程的环境里有这三项——ccnm 只去掉像凭据的变量（`*_TOKEN`、`*_API_KEY` 等），代理变量会一路传给它起的 Codex。用户带 `HTTPS_PROXY` 登录后，Codex 报 `Logged in using ChatGPT`，doctor 是 `可以用了（3 项不查……）`。探测时我多发了一次空的设备码请求，没绑定账号，15 分钟后自动过期。

**这一次**（候选构建 `31a7b26c…`，即 `242389d`；Codex 0.154.0 加按 digest `a68df7cc…` 核过的 `codex-code-mode-host`；实例不指定模型）：

| 步骤 | 结果 |
| --- | --- |
| `ccnm run p74codex --detached` | 会话信息写 `codex 在 systemd user service`；信任提示选 Yes；默认模型 `gpt-6-astra`，界面显示了账号的用量信息（经代理连到了 OpenAI） |
| 发消息：逐次跑 `["sh","-c","uname -s; printf P74-A-755429 > p74-a.txt"]` 和 `…P74-B… > p74-b.txt` | 约 10 秒后第一条弹出 Allow / Cancel，此时 Runtime 上只有 README |
| Allow | ccrun 执行：stdout `Linux`，`p74-a.txt` 内容 `P74-A-755429`；第二条照样弹出提示 |
| Cancel | 模型收到 `user cancelled MCP tool call`，如实回报两条结果；`p74-b.txt` 不存在，Runtime 上只有一条输出记录 |
| `ccnm stop … --session c9176b19…` | 退出 0；没有会话进程残留，写锁 `released`，`ccnm log` 记"被停止" |

额度：Codex 1 次（上限 2）。

**收尾**：隧道断开（hpsrv 上 17890 不再监听），bing 的代理环境变量撤掉，Controller 卸载，候选构建、`codex`、`codex-code-mode-host`、ccnm 配置、一次性密钥、`~/.ssh` 下本轮的文件、Codex 自建的 `~/.codex` 全删；ccrun 回到开始前。**保留**：tmux、bing 的 linger，以及用户的 Codex 登录（`~/.config/ccnm/agents/codex`，删不删由用户定）。

## 7. 还没做的

- **没发版**：v0.11.2 及之前的 Linux 包不能当 Agent。
- Claude Code 当 Linux Agent 没用真实模型跑过（逻辑相同：登录在 `~/.claude/.credentials.json`，Controller 回话即可）。
- 长期配代理的 systemd 附加配置（`dev.ccnm.controller.service.d/`）没实测，这次用的是 `set-environment`。
- **Windows**：两边都没做，要先另立设计。
