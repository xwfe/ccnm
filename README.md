# ccnm

**在你自己的电脑上用 Claude Code / Codex 写代码，看文件、改代码、跑测试却都发生在另一台机器上。**

项目放在服务器上、AI 登录在自己电脑（比如 Mac）上的人用得上它：代码不用搬，AI 登录也不用往服务器上放。

*ccnm lets you use Claude Code or Codex on your Mac while every file read, edit and command happens on another machine that holds the project, run there by a dedicated low-privilege account over SSH. The AI side runs on macOS, and on Linux since P74 (not yet released or verified on a real machine); the project side can be macOS or Linux x86\_64. Docs are in Chinese.*

## 它解决什么问题

项目在家里的 Linux 服务器或公司的台式机上，Claude / Codex 登录在你的 MacBook 上。常见的两种凑合办法都有代价：

| 办法 | 问题 |
| --- | --- |
| 在服务器上也登录 AI | 登录凭证多放一处；AI 跑的命令用的是你的账号，你能删的它都能删 |
| 把代码同步到 Mac 上改 | 两份代码要对齐；Mac 上没有服务器那套环境，测试跑不了或结果不一样 |

用 ccnm：AI 照常在 Mac 上跑，但它看文件、改代码、跑命令，都通过 SSH 交给项目那台机器去做，结果再传回来。项目机器上可以让一个专门的低权限账号替 AI 干活，它碰不到你的私钥和各种登录（这个账号要你自己建，见下面"该做的"）。

```text
你的 Mac（跑 AI）                                  项目机器（放代码）
Claude Code / Codex  ── 通过 SSH 发出"看/改/跑" ──▶  ccnm 用专门的账号去做
AI 登录只在这里       ◀──────────── 结果 ───────────  代码、Git、编译环境只在这里
```

## 两台机器各干什么

| | 跑 AI 的机器 | 放项目的机器 |
| --- | --- | --- |
| 文档里叫 | Agent Node | Runtime Node |
| 上面有什么 | Claude Code / Codex 和它的登录；一个常驻后台（Controller），负责拉起 AI | 项目代码、Git、编译测试工具；一个专门替 AI 跑命令的低权限账号（建议叫 `ccrun`，文档里叫执行账号） |
| 支持的系统 | macOS；Linux（带 systemd，新加的，见下） | macOS；Linux x86_64（实测 Debian 13，要 glibc 2.39 以上，比如 Ubuntu 24.04） |

**Linux 当跑 AI 的机器**是 v0.12.0 新加的：那个拉起 AI 的后台服务在 Mac 上靠 launchd，在 Linux 上装成 systemd 用户服务；Linux 上 AI 的登录存在普通文件里，不需要 Mac 那种图形登录。在一台 Debian 13 上从安装到用真实 Codex 改代码都跑通过；Claude Code 当 Linux 的跑 AI 机器还没用真实模型跑过。v0.11.2 及之前的包在 Linux 上会拒绝起会话。另外，跑 AI 的机器要能访问 OpenAI / Anthropic，在不支持的地区（比如中国大陆）要先配代理（[怎么配](docs/troubleshooting.md#登录-codex-报-device-code-request-failed-with-status-403-forbidden或会话里模型一直连不上)）。Linux 上有一件事要知道：默认你退出登录，systemd 会把后台服务和会话一起停掉，要常驻得开 linger（`sudo loginctl enable-linger <账号>`，详见[快速开始](docs/getting-started.md#3-初始化-agent-node)）。Windows 两边都还没做，要先单独设计。

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

- **代码在服务器，AI 在 Mac**：最主要的用法。
- **本机已经开着 Claude Code / Codex，想让它顺手改远端项目**：用 `ccnm mcp bridge`，不用 ccnm 另起 AI。默认关着，要在项目机器上先打开（[怎么开](docs/usage.md#把远端项目给已经在跑的-agent-用)）。
- **一句话的小活**：`ccnm my-project --print "修复 parser 测试"`，跑完结果直接打在终端里，中间不问。
- **让脚本派活**：`ccnm rpc`，不开网络端口，有现成的 Python 客户端（[协议说明](docs/protocol/README.md)）。
- **在手机上看进度、批命令**：用 PocketShell 这类 SSH App 连上 Mac，敲同样的命令（[说明](docs/usage.md#通过第三方终端使用)）。

不适合：

- **项目和 AI 在同一台机器上**：直接用 Claude Code / Codex 就好。ccnm 会拒绝这种配法。
- **要多个 AI 分工协作、自动拆任务、自动审查**：ccnm 只管"让一个 AI 在远端项目上干活"，安排活是另一个项目的事（[交接说明](docs/orchestrator-handoff.md)）。
- **要让代码一点都不出服务器**：AI 读到的代码会发给模型服务商。ccnm 分开的是机器和权限，不是数据。
- **要防数据外传**：ccnm 不管网络，防火墙要你自己在项目机器上配。

## 上手

准备（每一项怎么弄见[快速开始](docs/getting-started.md)）：

- 两台都装**同一个版本**的 ccnm；
- 两台之间能免密 SSH：项目机器上你的账号能连到 AI 那台；AI 那台能连到项目机器上替 AI 跑命令的账号；
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

`init` 后面的参数说明这台机器是谁：项目机器写 `--agent`（指向对面的 AI 机器），AI 机器写 `--runtime`（指向对面的项目机器）。SSH 别名只在定义它的那台机器上有效，所以两边各写各的。用 Codex、在一台机器上配多个 AI，见[使用说明](docs/usage.md)。输出默认中文，加 `--lang en` 换英文。

日常命令（前五条两台机器上都能敲，后两条只在项目机器上）：

```bash
ccnm my-project                          # 开会话并接上
ccnm attach my-project                   # 接回已有会话（简写 ccnm a）
ccnm ls                                  # 所有项目：在不在跑、跑了多久、工具通不通
ccnm log                                 # 跑过的会话，最新的在前
ccnm stop my-project                     # 结束会话
ccnm my-project --print "修复 parser 测试"   # 一问一答，不进 tmux
ccnm cleanup my-project                  # 看看会话留下了什么，再按提示加 --apply 删
```

**安装与升级**：去 [Releases](https://github.com/xwfe/ccnm/releases) 下载，Mac 取 `macos-universal`，Linux 取 `linux-x86_64`。用"新文件 + 改名"放进去：

```bash
tar -xzf ccnm-<版本>-macos-universal.tar.gz
mkdir -p ~/.local/bin
mv ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

用浏览器下载的包，Mac 会拦着不让运行，先执行 `xattr -d com.apple.quarantine ccnm`；用 `curl` 下载的不会。

升级时三件事别漏：先停掉所有会话；每台都换成同一个版本，放项目那台的执行账号（`ccrun`）名下那份也要换；跑 AI 的那台换完再敲一次 `ccnm controller install`，不然后台还是旧进程。完整步骤和回退见[运维：用发布包升级](docs/operations.md#用发布包升级一般就用这个)。

## 该做的和别做的

**该做**

- **常用交互会话的项目，打开 `allow_unattended_exec`**，免得每条命令都点一次确认：在项目机器的 `config.toml` 里那个 workspace 下写 `allow_unattended_exec = true`。开了之后命令执行前就没人看了，挡着它的只剩执行账号自己的权限（[细说](docs/configuration.md#allow_unattended_exec)）。一次性的小活也可以用 `--print`，那条路本来就不问。
- **真实项目先建一个专门替 AI 跑命令的账号**（建议叫 `ccrun`），写进项目机器配置的 `runtime_user`。不建的话，命令用你自己的账号跑——机器分开了，权限没分开。怎么建见[生产安全](docs/production-safety.md)。
- **改完配置或升级后，先跑 `ccnm doctor <项目>`。** 最后一行写"可以用了"就行。标"不查"的行是 doctor 本来就不查的（比如网络），看一眼说明；标"失败"或"没查"的要处理。
- **两台一起升级**，版本要一样，不一样时会报 `CCNM_E_VERSION`。

**别做**

- **别给替 AI 跑命令的账号任何 SSH 私钥、AI 登录或 sudo。** 它只该让别人连进来，自己不该能连出去。
- **别把 AI 登录拷到项目机器上。** ccnm 的前提就是登录只在 AI 那台。项目和 AI 登录在同一个账号下时，ccnm 默认不开会话（[两条出路](docs/getting-started.md#如果项目和-claude-登录在同一个账号下)）。
- **升级时别用 `cp` 覆盖正在用的 ccnm。** Apple Silicon 的 Mac 上会让程序签名失效，之后一运行就被系统杀掉（`Killed: 9`）。用上面的"新文件 + 改名"。
- **用 Codex 时别随手在 `/permissions` 里切 Approve for me。** 切了就是由 Codex 自己的自动审查决定放不放行（真机上连 `rm -f` 都放行），不是你；想不问，用上面的 `allow_unattended_exec`。
- **同一个项目别配两个状态目录**（比如两个不同的 `XDG_STATE_HOME`）。"同一时间只有一个会话能改代码"靠的是同一把锁，两个目录就成了两把互不知道的锁。
- **别开 `codex_exec_server`。** 这条路已经停止维护。

## 现在的状态

| | |
| --- | --- |
| 最新版本 | [v0.12.0](https://github.com/xwfe/ccnm/releases)（2026-10-07），每个版本改了什么写在 Releases 页 |
| 真机验过什么 | Mac 跑 AI → Debian 13 放项目（替 AI 跑命令的是专门的 `ccrun` 账号）：Claude 和 Codex 都用真实模型跑过——交互会话、一问一答、别的 AI 工具接入、脚本调用、安装升级回退。哪些**没验过**逐条写在[支持矩阵](docs/support-matrix.md) |

已知限制：

- **ccnm 不管网络。** AI 跑的命令能连到哪里，ccnm 不限制也没验证。
- **Codex 只认 0.154.0 这一个版本**（实测过的）；Claude Code 用你装的那个。
- **doctor 只看得出 Codex "登录过"**，登录失效要到会话的第一条消息才知道。
- **命令里自己脱离出去的后台进程**（比如用 `setsid` 起的守护进程）ccnm 停不掉，要按[运维手册](docs/operations.md#写入-guard-残留)手工收。
- **还在用 v0.11.0 的**：Codex 会话里有人选过一次 Approve for me，之后所有会话都不再问。升到 v0.11.1 以上就好；不升的话，去掉的办法见[排错手册](docs/troubleshooting.md#受管-codex-会话exec_command-每次都弹或者一次都不弹)。

会话里 AI 具体有哪些工具、能用哪些 skills 和 MCP，见[使用说明](docs/usage.md)和[配置说明](docs/configuration.md)。

## 文档

从[文档导航](docs/README.md)进；参与开发先读 [AGENTS.md](AGENTS.md)。

| 想做什么 | 看哪份 |
| --- | --- |
| 第一次搭起来 | [快速开始](docs/getting-started.md) → [配置](docs/configuration.md) → [使用](docs/usage.md) |
| 接真实项目、建专门的账号 | [生产安全](docs/production-safety.md) · [支持矩阵](docs/support-matrix.md) |
| 升级、断线、报错 | [运维](docs/operations.md) · [排错](docs/troubleshooting.md) |
| 写客户端或调度程序 | [公开协议](docs/protocol/README.md) · [执行接口交接](docs/orchestrator-handoff.md) |
| 了解内部设计 | [架构](docs/architecture.md) · [开发与发布](docs/development.md) · [计划与进度](docs/plan/README.md) · [研究记录](docs/research/) |

## 许可证

MIT，见 [LICENSE](LICENSE)。
