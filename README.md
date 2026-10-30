# ccnm

**Claude Code 登录在你这台电脑上，项目却在另一台机器上（比如一台服务器）？用 ccnm，这台的 Claude Code 直接去那台看代码、改代码、跑测试。**

那台上不用登录 AI，代码也不用来回同步。Codex 一样能用。两台都可以是 macOS 或 Linux，怎么搭配都行；放项目的那台也可以是 Windows。

*ccnm lets Claude Code or Codex run on the machine where you are logged in, while every file read, edit and command happens over SSH on the machine that holds the project. Either side can be macOS or Linux x86\_64; the project side can also be Windows. By default the project machine can SSH back to the AI machine, where the AI login lives; [production-safety.md](docs/production-safety.md#默认配置下项目机器能连回-ai-机器) has a one-line self-test and two ways to close it. Docs are in Chinese.*

## 用起来是什么样

在放项目的机器上敲（跑 AI 的那台上也能敲）：

```bash
ccnm my-project
```

1. 跑 AI 的那台开出 Claude Code（或 Codex），你的终端直接接进去。
2. 像平常一样聊。它看文件、搜代码、改代码、跑 `cargo test`，都在放项目的那台上做，结果传回来。
3. **默认每条命令跑之前都会停下来问你。** 嫌烦可以关掉，见[该做的](#该做的和别做的)第一条。
4. 要走开就直接关终端，AI 接着干；回来敲 `ccnm attach my-project`，做完 `ccnm stop my-project`。

```text
跑 AI 的机器                                          放项目的机器
Claude Code / Codex  ── 通过 SSH 发出"看/改/跑" ──▶  ccnm 在这台机器上去做
AI 登录只在这里       ◀──────────── 结果 ───────────  代码、Git、编译环境只在这里
```

## 你可能会遇到的事

- **一台 AI 机器额度用完了**：`ccnm my-project --on <另一台>`，换一台接着干，项目名和目录都不用改。上一个会话最后写的交接（做到哪、下一步干什么）会自动交给新会话；额度用完、模型没来得及写的话，交的是那段对话的结尾。之后的命令自动去新那台。同一个项目同一时间只有一台能改代码，旧的还开着时会先问你要不要停掉。加一台 AI 机器、名字怎么写，见[使用说明](docs/usage.md#换一台-ai-机器--on)。
- **网断了一会儿**：Claude 的会话开着时，AI 那台每 30 秒看一眼，工具断了自己接上，不用你管。你回来敲 `ccnm my-project` 或 `ccnm attach` 时，也会先接上再让你进。接不上会告诉你为什么（[排错手册](docs/troubleshooting.md#tools-down--会话看着在跑模型却什么都够不着)）。
- **对话太长了**：压缩几次后模型会建议换个新对话，并先写好交接；你 `/clear`，交接自动交给新对话。下次 `ccnm my-project -c` 接着上一次的对话开（[说明](docs/usage.md#长对话压缩几次之后换个新对话交接带过去)）。
- **想看额度还剩多少**：`ccnm usage`，每个 AI 登录用了多少、几点重置（[说明](docs/usage.md#查额度还剩多少ccnm-usage)）。
- **一句话的小活**：`ccnm my-project --print "修复 parser 测试"`，跑完结果直接打在终端里，中间不问。
- **本机已经开着 Claude Code / Codex**：用 `ccnm mcp bridge` 让它顺手改远端项目，不用 ccnm 另起 AI。默认关着，要先在项目机器上打开（[怎么开](docs/usage.md#把远端项目给已经在跑的-agent-用)）。
- **在手机上看进度、批命令**：用 PocketShell 这类 SSH App 连上跑 AI 的那台，敲同样的命令（[说明](docs/usage.md#通过第三方终端使用)）。
- **让脚本派活**：`ccnm rpc`，不开网络端口，有现成的 Python 客户端（[协议说明](docs/protocol/README.md)）。

## 为什么不直接在服务器上用 Claude Code

| 常见做法 | 问题 |
| --- | --- |
| 在服务器上也登录 AI | 登录凭证多放一处，而且就放在 AI 跑命令的那个账号里，一句 prompt 就能被读出去 |
| 把代码同步到本机改 | 两份代码要对齐；本机没有那套环境，测试跑不了或结果不一样 |

用 ccnm，AI 的登录只留在它自己那台。AI 的命令默认以你在项目机器上的账号跑，和你在那台上直接用 Claude Code 一样；想让它碰不到这个账号里的私钥和各种登录，可以另建一个低权限账号替它干活（可选，见[生产安全](docs/production-safety.md#要不要建专用账号)）。

**默认配置下有个口子：AI 能从项目机器连回跑 AI 的那台。** 上手时要让项目机器上你的账号能免密 SSH 到 AI 那台，而 AI 的命令默认就以这个账号跑，所以模型一条 `ssh` 就能过去，AI 的登录就在那边。上面表里第一行的风险，默认配置下并没有躲开。一条命令自测和两种堵法见[生产安全](docs/production-safety.md#默认配置下项目机器能连回-ai-机器)。

## 两台机器各干什么

| | 跑 AI 的机器 | 放项目的机器 |
| --- | --- | --- |
| 上面有什么 | Claude Code / Codex 和它的登录；一个后台服务（Controller），负责拉起 AI、断了替你接回 | 项目代码、Git、编译测试工具；AI 的命令在这里跑 |
| 系统 | macOS；Linux x86_64 | macOS；Linux x86_64；Windows x86_64（[装法](docs/windows-runtime-setup.md)） |
| 文档里叫 | Agent Node | Runtime Node |

"两台"其实是两个能互相 SSH 的账号：同一台机器上，用你的账号跑 AI、另建一个账号放项目也行。跑 AI 的那台可以有好几台，额度用完时用 `--on` 换。

每种搭配验到了哪一步：

| 跑 AI → 放项目 | 验到哪一步 |
| --- | --- |
| macOS → Linux | Claude 和 Codex 都用真实模型跑过：交互会话、一问一答、别的 AI 工具接入、脚本调用、安装升级回退 |
| macOS → macOS | Claude：作者日常在用；Codex：同一台 Mac 的两个账号上用真实模型跑通过脚本调用 |
| macOS → Windows | 一台 Win11 上用真实的 Claude Code 和 Codex 验收过 |
| Linux → Linux | 同一台 Debian 13 上的两个账号：Codex 用真实模型从安装到改代码跑通；Claude Code 当跑 AI 的一边还没用真实模型跑过 |
| Linux → macOS | 还没在真机上配过 |

逐项没验过的写在[支持矩阵](docs/support-matrix.md)。另外几件事先知道：

- Linux 要 glibc 2.39 以上（比如 Debian 13、Ubuntu 24.04），太旧时一运行就报 ``version `GLIBC_2.39' not found``。
- 跑 AI 的那台是 Linux 时，你一退出登录，systemd 默认就把后台服务和会话一起停掉，要常驻得开 linger（`sudo loginctl enable-linger <账号>`，见[快速开始](docs/getting-started.md#4-初始化跑-ai-的机器)）。
- 跑 AI 的那台要能访问 OpenAI / Anthropic；在不支持的地区（比如中国大陆）要先配代理（[怎么配](docs/troubleshooting.md#登录-codex-报-device-code-request-failed-with-status-403-forbidden或会话里模型一直连不上)）。

## 上手

准备（每一项怎么弄见[快速开始](docs/getting-started.md)）：

- 两台都装**同一个版本**的 ccnm，放在 `~/.local/bin/ccnm`（[怎么装](docs/getting-started.md#1-两台都装-ccnm)）；
- 两台之间能用钥匙免密 SSH，两个方向都要（用系统自带的 SSH 服务，别用 Tailscale SSH，[怎么配](docs/getting-started.md#2-配好双向-ssh)）；
- AI 那台上 Claude Code（或 Codex 0.154.0）已登录，装了 `tmux`；
- 项目机器上装了 `git`、`ripgrep` 和项目要用的编译工具。

在**项目机器**上：

```bash
ccnm init --agent <连 AI 那台用的 ssh 别名>
ccnm add /path/to/my-project      # 项目名取目录名：my-project
```

在 **AI 那台**上：

```bash
ccnm init --runtime <连项目机器用的 ssh 别名>
ccnm controller install           # 装那个负责拉起 AI 的后台服务
```

回到**项目机器**：

```bash
ccnm doctor my-project            # 只读体检，逐项说哪里没配好
ccnm my-project                   # 开始
```

参数怎么写、第一次启动会问什么、用 Codex 要多做哪几步，都在[快速开始](docs/getting-started.md)。输出默认中文，加 `--lang en` 换英文。

## 日常命令

```bash
ccnm my-project                          # 开会话并接上
ccnm my-project -c                       # 接着上一次的对话开（只支持 Claude）
ccnm my-project --on <另一台>             # 换一台 AI 机器开（在项目机器上敲）
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通
ccnm usage                               # AI 登录的额度用了多少、几点重置
ccnm stop my-project                     # 结束会话
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux
ccnm handoff my-project                  # 下次新会话会带上什么交接（在项目机器上敲）
ccnm log                                 # 跑过的会话，最新的在前
```

完整用法见[使用说明](docs/usage.md)。

## 该做的和别做的

**该做**

- **常用交互会话的项目，打开 `allow_unattended_exec`**，免得每条命令都点一次确认（[怎么开](docs/getting-started.md#不想每条命令都点确认)）。开了之后命令执行前就没人看了，挡着它的只剩替它跑命令那个账号的权限。
- **改完配置或升级后，先跑 `ccnm doctor <项目>`。** 最后一行以"可以用了"开头就行；标"失败"或"没查"的行要处理（[每一行怎么读](docs/troubleshooting.md#doctor-的表怎么读)）。
- **升级时三件事别漏**：先停掉会话；所有装了 ccnm 的机器换同一个版本，另建了替 AI 跑命令的账号的，它名下那份也换；跑 AI 的那几台换完再敲一次 `ccnm controller install`，不然后台还是旧的。每个版本改了什么看 [Releases 页](https://github.com/xwfe/ccnm/releases)，步骤见[运维：用发布包升级](docs/operations.md#用发布包升级一般就用这个)。

**别做**

- **别把 AI 登录拷到项目机器上。** 拷过去之后，模型在那边跑的命令就能把它读出来。
- **升级时别用 `cp` 覆盖正在用的 ccnm。** Apple Silicon 的 Mac 上会让程序签名失效，之后一运行就被系统杀掉（`Killed: 9`）。用"新文件 + 改名"（[怎么做](docs/getting-started.md#1-两台都装-ccnm)）。
- **别指望 Claude Code 的后台 agent 改文件。** 同一个项目同一时间只有一个会话能改，主会话占着，后台 agent 只能读代码、查问题，要改的地方回主会话改（[说明](docs/troubleshooting.md#claude-code-的后台-agent-只能读项目改不了文件)）。
- **用 Codex 时别随手在 `/permissions` 里切 Approve for me。** 切了就是 Codex 自己的自动审查决定放不放行（真机上连 `rm -f` 都放行），不是你。
- **别让两个账号各自跑 ccnm 去服务同一个项目目录。** "同一时间只有一个会话能改代码"靠的是一把记在 ccnm 状态目录里的锁，两个账号就是两把互不知道的锁。

## 不适合的情况

- **项目和 AI 在同一个账号下**：直接用 Claude Code / Codex 就好，ccnm 会拒绝这种配法。
- **要多个 AI 分工、自动拆任务、自动审查**：ccnm 只管"让一个 AI 在远端项目上干活"，额度用完也不替你自动换机器（[交接说明](docs/orchestrator-handoff.md)）。
- **要代码一点都不出服务器**：AI 读到的代码会发给模型服务商。ccnm 分开的是机器和权限，不是数据。
- **要防数据外传**：ccnm 不管网络，AI 跑的命令能连到哪里由你在项目机器上用防火墙管。

还有几条限制：

- **Codex 只认 0.154.0 这一个版本**（实测过的）；Claude Code 用你装的那个。
- **doctor 只看得出 Codex "登录过"**，登录失效要到会话的第一条消息才知道。
- **命令里自己脱离出去的后台进程**（比如用 `setsid` 起的守护进程）：项目机器是 Linux 时会话结束一并收掉；是 macOS 时 ccnm 停不掉，可能挡住下一个会话或接着改文件，处理见[运维手册](docs/operations.md#写入-guard-残留)和[排错手册](docs/troubleshooting.md#会话已经结束工作区却还在被写)。

## 文档

从[文档导航](docs/README.md)进；参与开发先读 [AGENTS.md](AGENTS.md)。

| 想做什么 | 看哪份 |
| --- | --- |
| 第一次搭起来 | [快速开始](docs/getting-started.md) → [配置](docs/configuration.md) → [使用](docs/usage.md) |
| 要不要隔离、怎么建专门的账号 | [生产安全](docs/production-safety.md) · [支持矩阵](docs/support-matrix.md) |
| 升级、断线、报错 | [运维](docs/operations.md) · [排错](docs/troubleshooting.md) |
| 写客户端或调度程序 | [公开协议](docs/protocol/README.md) · [执行接口交接](docs/orchestrator-handoff.md) |
| 了解内部设计 | [架构](docs/architecture.md) · [开发与发布](docs/development.md) · [计划与进度](docs/plan/README.md) · [研究记录](docs/research/) |

## 许可证

MIT，见 [LICENSE](LICENSE)。
