# ccnm

**让 Claude Code / Codex 在一台机器上跑，它看文件、改代码、跑命令却都在另一台机器上做。**

一台机器跑 AI，AI 的登录只放在这里；另一台放项目，代码、Git、编译环境只在这里。两台都可以是 macOS 或 Linux：Mac 跑 AI、Linux 服务器放项目最常见，反过来、两台都是 Mac 或都是 Linux 也都实现了，每种搭配验到哪一步见[下面的表](#两台机器各干什么)。

*ccnm runs Claude Code or Codex on one machine while every file read, edit and command happens over SSH on another machine that holds the project. Either machine can be macOS or Linux x86\_64, in any combination; Windows is not supported yet. On the project machine, commands run as your own account by default, or as a dedicated low-privilege one if you set it up. Docs are in Chinese.*

## 它解决什么问题

项目在一台机器上（家里的 Linux 服务器、公司的台式机、另一台 Mac），Claude / Codex 登录在你平时用的那台上。常见的两种凑合办法都有代价：

| 办法 | 问题 |
| --- | --- |
| 在放项目的机器上也登录 AI | 登录凭证多放一处，而且就放在 AI 跑命令的那个账号里，一句 prompt 就能被读出去 |
| 把代码同步到跑 AI 的机器上改 | 两份代码要对齐；这边没有那套环境，测试跑不了或结果不一样 |

用 ccnm：AI 照常在它那台机器上跑，但它看文件、改代码、跑命令，都通过 SSH 交给放项目的机器去做，结果再传回来。命令默认以你在项目机器上的账号跑，和你在那台机器上直接用 Claude Code 一样；想让它碰不到那个账号里的私钥和各种登录，可以另建一个专门的低权限账号替它干活（可选，见[生产安全](docs/production-safety.md#要不要建专用账号)）。

```text
跑 AI 的机器（macOS / Linux）                       放项目的机器（macOS / Linux）
Claude Code / Codex  ── 通过 SSH 发出"看/改/跑" ──▶  ccnm 在这台机器上去做
AI 登录只在这里       ◀──────────── 结果 ───────────  代码、Git、编译环境只在这里
```

## 两台机器各干什么

| | 跑 AI 的机器 | 放项目的机器 |
| --- | --- | --- |
| 文档里叫 | Agent Node | Runtime Node |
| 上面有什么 | Claude Code / Codex 和它的登录；一个常驻后台（Controller），负责拉起 AI（Mac 上是 launchd 服务，Linux 上是 systemd 用户服务） | 项目代码、Git、编译测试工具。AI 的命令以 SSH 登进来的那个账号跑（文档里叫执行账号），默认就是你自己的 |
| 系统 | macOS；Linux x86_64 | macOS；Linux x86_64 |

"两台"指的是两个 SSH 能互相连上的账号，不一定是两台物理机：同一台机器上，用你的账号跑 AI、另建一个账号放项目也行。Windows 两边都还没做，要先单独设计。

每种搭配验到了哪一步：

| 跑 AI → 放项目 | 验到哪一步 |
| --- | --- |
| macOS → Linux | Claude 和 Codex 都用真实模型跑过：交互会话、一问一答、别的 AI 工具接入、脚本调用、安装升级回退（Debian 13，执行账号是专门的 `ccrun`） |
| macOS → macOS | Claude：作者日常在用，两台 Mac，真实模型、交互会话；Codex：同一台 Mac 的两个账号上用真实模型跑通过脚本调用 |
| Linux → Linux | 同一台 Debian 13 上的两个账号：Codex 用真实模型从安装到改代码跑通；Claude Code 当跑 AI 的一边还没用真实模型跑过 |
| Linux → macOS | 还没在真机上配过 |

逐项没验过的写在[支持矩阵](docs/support-matrix.md)。Linux 上有两件事先知道：

- 要 glibc 2.39 以上（比如 Debian 13、Ubuntu 24.04），太旧时一运行就报 ``version `GLIBC_2.39' not found``。
- 跑 AI 的那台默认你一退出登录，systemd 就把后台服务和会话一起停掉，要常驻得开 linger（`sudo loginctl enable-linger <账号>`，见[快速开始](docs/getting-started.md#4-初始化跑-ai-的机器)）。

不管哪种系统，跑 AI 的机器都要能访问 OpenAI / Anthropic；在不支持的地区（比如中国大陆）要先配代理（[怎么配](docs/troubleshooting.md#登录-codex-报-device-code-request-failed-with-status-403-forbidden或会话里模型一直连不上)）。

还有两个词会经常看到：

- **workspace**：在项目机器上登记过的一个项目目录，起个名字，比如 `my-project`。命令里写的都是这个名字。
- **会话**：一次 Claude / Codex 运行，开在 AI 那台机器的 tmux 里。关掉终端它还在，随时接回。

## 用起来是什么样

1. 在项目机器上敲 `ccnm my-project`。
2. AI 那台机器上开出 Claude Code（或 Codex），你的终端直接接进去。
3. 像平常一样聊。它要看文件、搜代码、改代码、跑 `cargo test`，都在项目机器上做：看和改只限项目目录，命令能碰到什么取决于替它跑命令的那个账号的权限。它自带的读文件、跑命令功能是关掉的。
4. **默认每次要跑命令，它会先停下来问你。** 嫌一条条点烦可以关掉，见下面[该做的](#该做的和别做的)第一条。
5. 要走开就直接关终端，AI 接着干；回来敲 `ccnm attach my-project`。做完 `ccnm stop my-project`。

## 适合和不适合

适合：

- **代码在一台机器，AI 登录在另一台**：最主要的用法。
- **本机已经开着 Claude Code / Codex，想让它顺手改远端项目**：用 `ccnm mcp bridge`，不用 ccnm 另起 AI。默认关着，要在项目机器上先打开（[怎么开](docs/usage.md#把远端项目给已经在跑的-agent-用)）。
- **一句话的小活**：`ccnm my-project --print "修复 parser 测试"`，跑完结果直接打在终端里，中间不问。
- **让脚本派活**：`ccnm rpc`，不开网络端口，有现成的 Python 客户端（[协议说明](docs/protocol/README.md)）。
- **在手机上看进度、批命令**：用 PocketShell 这类 SSH App 连上跑 AI 的那台，敲同样的命令（[说明](docs/usage.md#通过第三方终端使用)）。

不适合：

- **项目和 AI 在同一个账号下**：直接用 Claude Code / Codex 就好。ccnm 会拒绝这种配法（同一台机器的两个账号可以，见上）。
- **要多个 AI 分工协作、自动拆任务、自动审查**：ccnm 只管"让一个 AI 在远端项目上干活"，安排活是另一个项目的事（[交接说明](docs/orchestrator-handoff.md)）。
- **要让代码一点都不出服务器**：AI 读到的代码会发给模型服务商。ccnm 分开的是机器和权限，不是数据。
- **要防数据外传**：ccnm 不管网络，防火墙要你自己在项目机器上配。

## 上手

准备（每一项怎么弄见[快速开始](docs/getting-started.md)）：

- 两台都装**同一个版本**的 ccnm，放在 `~/.local/bin/ccnm`（[怎么装](docs/getting-started.md#1-两台都装-ccnm)）；
- 两台之间能免密 SSH：项目机器上你的账号能连到 AI 那台；AI 那台能连到项目机器（连进去的那个账号就是替 AI 跑命令的账号，用你自己的就行）；
- AI 那台上 Claude Code（或 Codex 0.154.0）已登录，装了 `tmux`；
- 项目机器上装了 `git`、`ripgrep` 和项目要用的编译工具。

在**项目机器**上：

```bash
ccnm init --agent <连 AI 那台用的 ssh 别名>
cd /path/to/project
ccnm workspace add my-project
```

在 **AI 那台**上：

```bash
ccnm init --runtime <连项目机器用的 ssh 别名>
ccnm controller install        # 装那个负责拉起 AI 的后台服务
```

回到**项目机器**：

```bash
ccnm doctor my-project      # 只读体检，逐项说哪里没配好
ccnm my-project             # 开始
```

`init` 的参数和别名怎么写、第一次启动会问什么、用 Codex 要多做哪几步，都在[快速开始](docs/getting-started.md)。输出默认中文，加 `--lang en` 换英文。

日常命令（前五条两台机器上都能敲，后两条只在项目机器上）：

```bash
ccnm my-project                          # 开会话并接上
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通（在 AI 那台只看得到它上面的会话）
ccnm log                                 # 跑过的会话，最新的在前
ccnm stop my-project                     # 结束会话
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux
ccnm cleanup my-project                  # 看看会话留下了什么，再按提示加 --apply 删
```

**安装**见[快速开始](docs/getting-started.md#1-两台都装-ccnm)。**升级**时三件事别漏：先停掉所有会话；每台都换成同一个版本，放项目那台如果另建了执行账号（比如 `ccrun`），它名下那份也要换；跑 AI 的那台换完再敲一次 `ccnm controller install`，不然后台还是旧进程。完整步骤和回退见[运维：用发布包升级](docs/operations.md#用发布包升级一般就用这个)。

## 该做的和别做的

**该做**

- **常用交互会话的项目，打开 `allow_unattended_exec`**，免得每条命令都点一次确认（[怎么开](docs/getting-started.md#不想每条命令都点确认)）。开了之后命令执行前就没人看了，挡着它的只剩执行账号自己的权限。一次性的小活也可以用 `--print`，那条路本来就不问。
- **想让 AI 的命令碰不到你账号里的私钥和登录**，看[生产安全：要不要建专用账号](docs/production-safety.md#要不要建专用账号)。不建也能用：命令就以你自己的账号跑，机器分开了、权限没分开。
- **改完配置或升级后，先跑 `ccnm doctor <项目>`。** 最后一行以"可以用了"开头就行；标"失败"或"没查"的行要处理，标"注意"和"不查"的不挡你用（[每一行怎么读](docs/troubleshooting.md#doctor-的表怎么读)）。
- **两台一起升级**，版本要一样，不一样时会报 `CCNM_E_VERSION`。

**别做**

- **别把 AI 登录拷到项目机器上。** 登录只在 AI 那台是 ccnm 的前提；拷过去之后，模型在那边跑的命令就能把它读出来。
- **升级时别用 `cp` 覆盖正在用的 ccnm。** Apple Silicon 的 Mac 上会让程序签名失效，之后一运行就被系统杀掉（`Killed: 9`）。用[快速开始](docs/getting-started.md#1-两台都装-ccnm)里的"新文件 + 改名"。
- **用 Codex 时别随手在 `/permissions` 里切 Approve for me。** 切了就是由 Codex 自己的自动审查决定放不放行（真机上连 `rm -f` 都放行），不是你；想不问，用上面的 `allow_unattended_exec`。
- **别让两个账号（或两份不同的 `XDG_STATE_HOME`）各自跑 ccnm 去服务同一个项目目录。** "同一时间只有一个会话能改代码"靠的是一把锁，锁记在各自的状态目录里，就成了两把互不知道的锁。

## 现在的状态

| | |
| --- | --- |
| 最新版本 | [v0.14.0](https://github.com/xwfe/ccnm/releases)（2026-10-09），每个版本改了什么写在 Releases 页 |
| 真机验过什么 | 按搭配见上面[两台机器各干什么](#两台机器各干什么)；哪些**没验过**逐条写在[支持矩阵](docs/support-matrix.md) |
| 从旧版升到 v0.14.0 | 配置里还写着 `codex_exec_server` 的，删掉那一行，否则配置读不进来（[报错长这样](docs/troubleshooting.md#升级后-ccnm-报-unknown-field-codex_exec_server)）。另外两处新东西不用改配置：项目机器是 Linux 时，会话结束会收掉命令留下的后台进程；另建了执行账号时，doctor 多一行 `以你身份生效的文件` |

已知限制：

- **ccnm 不管网络。** AI 跑的命令能连到哪里，ccnm 不限制也没验证。
- **Codex 只认 0.154.0 这一个版本**（实测过的）；Claude Code 用你装的那个。
- **doctor 只看得出 Codex "登录过"**，登录失效要到会话的第一条消息才知道。
- **命令里自己脱离出去的后台进程**（比如用 `setsid` 起的守护进程）：项目机器是 Linux 时，会话结束时一并收掉。是 macOS 时 ccnm 停不掉：还攥着命令输出的，写锁留着、下一个会话被挡，按[运维手册](docs/operations.md#写入-guard-残留)手工收；完全脱离的，写锁照常交出，它可能还在改文件，要自己找到它结束掉（[怎么找](docs/troubleshooting.md#会话已经结束工作区却还在被写)）。

会话里 AI 具体有哪些工具、能用哪些 skills 和 MCP，见[使用说明](docs/usage.md)和[配置说明](docs/configuration.md)。

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
