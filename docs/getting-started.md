# 快速开始

本页从零搭起来：一台机器放项目，一台机器跑 AI。默认用 Claude Code，用 Codex 的看最后一节。"Agent Node""Runtime Node""执行账号"这几个词，见 [README 的"两台机器各干什么"](../README.md#两台机器各干什么)：Agent Node 就是跑 AI 的那台，Runtime Node 就是放项目的那台。

## 先准备

**跑 AI 的机器**（Agent Node）：

- macOS，或 x86_64 的 Linux（要有 systemd）；
- Claude Code 已经登录（用 Codex 见[最后一节](#7-用-codex)）；
- 装了 `tmux`；
- 能访问 Anthropic / OpenAI。在不支持的地区（比如中国大陆）要先配代理，见[排错手册](troubleshooting.md#登录-codex-报-device-code-request-failed-with-status-403-forbidden或会话里模型一直连不上)。

**放项目的机器**（Runtime Node）：

- macOS，或 x86_64 的 Linux；
- 项目目录、`git`，以及项目自己要用的编译工具；
- `ripgrep`（命令名 `rg`）。AI 搜代码靠它，doctor 不查这一项。用 brew 或 apt 装最省事；装好后在跑 AI 的那台敲 `ssh <连项目那台的别名> 'command -v rg'`，要打出一个路径（ccnm 用的是这种非交互 SSH 看到的 `PATH`，你登录进去能用不算数）。没找到的话，模型第一次搜代码时报 `ripgrep is not installed on the Runtime Node…`。

两台都可以是 macOS 或 Linux，任意搭配。每种搭配验到哪一步，见 [README 的表](../README.md#两台机器各干什么)。

ccnm 不替你配 Tailscale、VPN、SSH 密钥或 `~/.ssh/config`，它只用已经能连通的 SSH 别名。

## 1. 两台都装 ccnm

两台装**同一个版本**。去 [Releases](https://github.com/xwfe/ccnm/releases) 看最新版本号，Mac 下 `macos-universal`（arm64 和 x86_64 都能跑），Linux 下 `linux-x86_64`。两个包都能当任意一边。

```bash
v=<版本号>; p=macos-universal                # Linux 上 p=linux-x86_64
curl -fLO https://github.com/xwfe/ccnm/releases/download/v$v/ccnm-$v-$p.tar.gz
tar -xzf ccnm-$v-$p.tar.gz                   # 包里只有一个 ccnm
mkdir -p ~/.local/bin
install -m 755 ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
~/.local/bin/ccnm --version                  # 两台打出来要一样
```

两件事要注意：

- **必须放在 `~/.local/bin/ccnm`。** 另一台是经 SSH 按这个路径调它的。放在别处时，发起调用的那一台会报 `~/.local/bin/ccnm not found on <别名> (the login shell exited 127)`，要在**发起调用那一台**的配置里写 [`ccnm_bin`](configuration.md#node-的其他字段) 指过来。
- **`~/.local/bin` 要在 `PATH` 里。** 否则敲 `ccnm` 报 `command not found: ccnm`（bash 是 `ccnm: command not found`）。在 shell 的配置文件（比如 `~/.zshrc`）里加一行 `export PATH="$HOME/.local/bin:$PATH"`。

Linux 的包要 glibc 2.39 以上（比如 Debian 13、Ubuntu 24.04），太旧时一运行就报 ``version `GLIBC_2.39' not found``。Mac 上用浏览器下载的包会被系统拦着不让运行，先执行 `xattr -d com.apple.quarantine ccnm`；用上面的 `curl` 下载不会。

## 2. 先验证双向 SSH

在放项目的机器上：

```bash
ssh -o BatchMode=yes <连 AI 那台的别名> true
```

在跑 AI 的机器上：

```bash
ssh -o BatchMode=yes <连项目那台的别名> true
```

成功的样子是什么都不打、马上返回。ccnm 连 SSH 时不允许弹任何提示：要输密码、私钥口令没交给 ssh-agent，都直接算失败，所以先在这里过一遍。常见失败：

- `Permission denied (publickey)`：对面账号的 `~/.ssh/authorized_keys` 里没有你的公钥。
- `Host key verification failed`：第一次连这台，先不带 `-o BatchMode=yes` 手动连一次，确认主机指纹。

"别名"就是 `~/.ssh/config` 里 `Host` 后面那个名字：

```text
Host work
    HostName 192.0.2.10
    User me
```

从跑 AI 的机器连过去的那个账号，就是替 AI 跑命令的账号（文档里叫执行账号）。用你自己的就行；想另建一个低权限账号，见[生产安全](production-safety.md#要不要建专用账号)。

## 3. 初始化放项目的机器

```bash
ccnm init --agent <连 AI 那台的别名>
cd /path/to/project
ccnm workspace add my-project
```

**两台各初始化一次，每次只给一个别名。** 给哪个参数，同时也说明了这台机器是谁：放项目的机器给 `--agent`（指向对面跑 AI 的那台），跑 AI 的机器给 `--runtime`。两个一起给会被拒。

`workspace add` 后面的名字就是以后命令里写的项目名，只能用字母、数字、`_` 和 `-`，开头是字母或数字。不写名字就用目录名，目录名里有 `.`、空格或中文时会报 `name must be [A-Za-z0-9][A-Za-z0-9_-]*`，自己给个名字就行。

配置写在 `~/.config/ccnm/config.toml`（用 `--config` 或环境变量 `CCNM_CONFIG` 可以换），这台写出来的长这样：

```toml
this = "runtime"          # 我是 runtime 这个 node

[nodes.runtime]           # 我自己，不需要 ssh

[nodes.agent]
ssh = "agent-ssh-alias"   # 从我这里连 agent，用这个别名
```

`ssh` 永远是"**从读这个文件的机器出发**，连那个 node 用的别名"。两台各写各的，因为别名只在定义它的那台机器的 `~/.ssh/config` 里有意义：你家里那台管工作机叫 `work`，工作机管你家里那台叫 `home`，没有一个全局的名字。

项目只在这台机器上登记。跑 AI 的那台不登记，每次去问这台。

## 4. 初始化跑 AI 的机器

```bash
ccnm init --runtime <连项目那台的别名>
ccnm controller install        # 装负责拉起 AI 的后台服务（Controller）
ccnm controller status
```

这边写出来的多一行 `runtime_node`：

```toml
this = "agent"
runtime_node = "runtime"  # 我不存项目列表，问这个 node

[nodes.agent]

[nodes.runtime]
ssh = "runtime-ssh-alias"
```

`runtime_node` 这一行由 `init` 写，别删。没有它，一台刚初始化、还没加项目的项目机器，和这台的配置长得一模一样，ccnm 就分不清这台是谁。

**macOS 上**，Controller 是一个 launchd 服务，跑在图形登录会话里：这样哪怕请求是从 SSH 来的，Claude Code 也能用钥匙串里的登录。所以这台 Mac 至少要有人在屏幕前登录过一次（锁屏没关系）。只有 SSH 登录、没有图形登录时，doctor 的 `Controller` 行会写 `Background`，`Claude authentication` 那一行是"没查"，见[排错手册](troubleshooting.md#doctor-的-controller-行失败写着-backgroundmacos)。

**Linux 上**，Controller 装成 systemd 用户服务（`~/.config/systemd/user/dev.ccnm.controller.service`），不需要图形登录：Claude 和 Codex 在 Linux 上把登录存在文件里，任何会话都读得到。三件事要知道：

- **`ccnm controller install` 要在这个账号用 SSH 登录进来的会话里跑。** `su` 或 `sudo -u` 进来的会话通常报 `Failed to connect to bus`，见[排错手册](troubleshooting.md#linux-上-ccnm-controller-install-报-failed-to-connect-to-bus)。
- **这台机器要能访问 AI 服务。** 出口在不支持的地区时，登录会报 403、会话里模型连不上，见[排错手册](troubleshooting.md#登录-codex-报-device-code-request-failed-with-status-403-forbidden或会话里模型一直连不上)。
- **默认这个账号最后一次退出登录时，systemd 会停掉 Controller 和它起的所有会话。** 要它常驻，开 linger（说白了就是"没人登录也保留这个账号的用户服务"）：`sudo loginctl enable-linger <账号>`。没开时，`controller install`、`controller status` 和 doctor 的 `Controller` 行都会提示。

## 5. 跑 doctor

回到放项目的机器：

```bash
ccnm doctor my-project
```

最后一行以"可以用了"开头就能开始，比如：

```text
可以用了（2 项不查，原因写在标“不查”的行里）
```

标"失败"或"没查"的行要处理，每一行都写了原因和修法。标"注意"的行不挡你用：最常见的是命令以你自己的账号跑，那几行在告诉你模型够得到哪些东西；想把它们隔开，见[生产安全：要不要建专用账号](production-safety.md#要不要建专用账号)。每一行怎么读、中英文行名怎么对应，见[排错手册：doctor 的表怎么读](troubleshooting.md#doctor-的表怎么读)。

doctor 只读，不会替你建账号、改权限或登录 Claude。

## 6. 开始用

```bash
ccnm my-project
```

**第一次起会话，Claude Code / Codex 会先问几句**，答一次，同一个项目以后不再问：

- **"是否信任这个目录"**：选信任。问的是 ccnm 在跑 AI 那台上给这个项目建的空占位目录（`~/.local/state/ccnm/workspaces/<项目名>`），不是你的项目。Claude 默认选中的是 "No, exit"，要先按一次下箭头，直接回车会话就退出了。
- **Claude Code 问"要不要把 auto mode 设成默认权限模式"**：选 "No"。选 "Yes" 改的是那个账号上 Claude Code 的全局默认，你在那台机器上直接用 Claude 时也会跟着变；ccnm 起会话时自己指定权限模式，用不着它。

用 `--detached` 起的会话要先 `ccnm attach my-project` 答完这几句，工具才连得上；答完之前 `ccnm status` 显示 `TOOLS DOWN`。

在跑 AI 的那台敲同一条命令也行：它先问项目机器这个名字对应哪个目录，再在本机起会话、接上。`--print`（一问一答）例外，只能在项目机器上敲，在 AI 那台敲会报 `--print has to be run where the projects are; ssh there and run it`。

日常命令和会话里 AI 能做什么，见[使用说明](usage.md)。

### 不想每条命令都点确认

交互会话默认每跑一条命令都会停下来问你一次。平时就用交互会话的项目，建议在**放项目的机器**的 `config.toml` 里，`ccnm workspace add` 写出来的那个 `[workspaces.my-project]` 下面加一行：

```toml
allow_unattended_exec = true
```

之后新起的会话就不问了，项目 skill 里要求加载时跑的命令和 `hooks` 也会跟着跑（[说明](usage.md#项目自带的-skills)）。别另起一个同名的 `[workspaces.my-project]` 再写这一行：TOML 不允许同一张表出现两次，整份配置会读不进来。开了之后少了什么、怎么收回、doctor 里 `命令审批` 那一行为什么一直是"注意"，见[配置说明](configuration.md#allow_unattended_exec)。

## 7. 用 Codex

**1. 在跑 AI 的机器上装 Codex 0.154.0。** 只认这一个版本（实测过的），别的版本 doctor 报 `Codex <版本> has not been measured; this adapter requires 0.154.0`。从 [openai/codex 的 rust-v0.154.0 发布页](https://github.com/openai/codex/releases/tag/rust-v0.154.0)下**两个**包：`codex` 本体，和 `codex-code-mode-host`。后者必须和 `codex` 放在同一个目录：不给实例指定模型时，ccnm 用的是 Codex 的 Code Mode，少了它，会话里模型一个工具都拿不到。包里的文件名带着平台，要改名：

```bash
t=aarch64-apple-darwin          # Intel Mac 是 x86_64-apple-darwin，Linux 是 x86_64-unknown-linux-musl
base=https://github.com/openai/codex/releases/download/rust-v0.154.0
curl -fLO $base/codex-$t.tar.gz && curl -fLO $base/codex-code-mode-host-$t.tar.gz
tar -xzf codex-$t.tar.gz && tar -xzf codex-code-mode-host-$t.tar.gz
install -m 755 codex-$t ~/.local/bin/codex
install -m 755 codex-code-mode-host-$t ~/.local/bin/codex-code-mode-host
~/.local/bin/codex --version    # 要打出 0.154.0
```

ccnm 按这个顺序找 `codex`：Controller 的 `PATH`，然后是 `~/.local/bin`、`~/.cargo/bin`、`/opt/homebrew/bin`、`/usr/local/bin`。

**2. 登录到 ccnm 自己的 Codex 目录。** ccnm 不用你日常的 `~/.codex`，它用 `~/.config/ccnm/agents/codex/`。这个目录要你自己先建好，而且只能你自己能进（权限 0700），否则 doctor 报 `dedicated Agent home must be private…`。然后对它单独登录一次，用 0.154.0 这份 Codex 登（更新的版本可能顺手升级目录里的文件，之后 0.154.0 可能就读不了了）：

```bash
mkdir -p -m 700 ~/.config/ccnm/agents/codex
CODEX_HOME=~/.config/ccnm/agents/codex ~/.local/bin/codex login
```

Linux 上没有浏览器时加 `--device-auth`；要代理时在前面加 `HTTPS_PROXY=http://<代理地址>`。别从 `~/.codex` 把 `auth.json` 拷过来，要单独登录。

**3. 两边配置各加几行。** 跑 AI 的机器上，定义一个 Codex 实例：

```toml
[agents.codex-main]
provider = "codex"
profile_ref = "default"
```

放项目的机器上，把这个项目的 `agent_node = "agent"` 换成指向这个实例（两个不能同时写）：

```toml
[workspaces.my-project]
agent = { node = "agent", instance = "codex-main" }
root = "/path/to/project"
```

然后照常 `ccnm doctor my-project`、`ccnm my-project`。同一台机器上 Claude、Codex 都配、按需切换，见[使用说明：选择 Agent Instance](usage.md#选择-agent-instance)；改写法的细节和容易踩的坑见[运维：配置迁移](operations.md#配置迁移legacy--agent-instance)。

## 升级

见[运维：用发布包升级](operations.md#用发布包升级一般就用这个)。自己从源码编译部署的，见同一页的[从源码部署](operations.md#从源码部署开发用)。
