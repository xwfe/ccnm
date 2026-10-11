# 默认配置下项目机器能连回 AI 机器（2026-10-11）

## 结论

1. **默认配置下，模型一条 `exec_command` 就能连回 AI 机器。** 作者日用的两台 Mac、v0.14.0 发布二进制、Runtime 不写 `runtime_user`（P78 起的默认共用账号）：`exec_command` 里跑 `ssh -o BatchMode=yes fodelf true`，退出码 0。原因是两条默认凑在一起：[快速开始第 2 节](../getting-started.md#2-配好双向-ssh)要你让项目机器上的账号免密 SSH 到 AI 机器；命令默认又正好以这个账号跑。AI 的登录和你在 AI 机器上的日常数据都在那台，[README](../../README.md#它解决什么问题) 说 ccnm 解决的"一句 prompt 就能把登录读出去"，在默认配置下没解决，影响面还从项目机器扩到了 AI 机器。
2. **这对机器上，删钥匙也挡不住。** fodelf 的 22 端口是 Tailscale SSH，按 tailnet 身份放行：exec 里把私钥和 ssh-agent 都关掉（`-o PubkeyAuthentication=no -o IdentityAgent=none`）照样退出 0。同样的参数连普通 OpenSSH（本机的 sshd）是 `Permission denied`、255。这和 P7.5 在 `ccrun` 上撞到的是同一件事（[生产安全：网络出口](../production-safety.md#网络出口)），所以专用账号在这种部署下也挡不住。同日按用户要求关掉了 fodelf 的 Tailscale SSH，之后不用钥匙就连不上了（第 9 节）。
3. **只从 AI 机器发起时，ccnm 确实不需要项目机器连回 AI 机器。** 项目机器那份临时配置里把 AI 机的别名写成解析不了的名字，在 fodelf 上 `init`、`doctor`、`run --detached`、`status`、`attach`、`ls`、`stop`、`log` 全部正常，会话期间本机只有入站的 `mcp-serve`、没有任何发往 AI 机器的 ssh，结束后写锁 `released`。代价是 `--print`、`cleanup`、`ccnm rpc` 用不了，项目机器上的会话命令全部报 `CCNM_E_AGENT_UNREACHABLE`（第 4.2 节）。
4. **doctor 看不出这个口子**：`SSH 私钥` 那一行只看执行账号名下有没有私钥，不去试连。

默认值没改：用户定默认不需要 `ccrun`，见第 7 节。

## 1. 拓扑和做法

| 机器 | 在这次里的角色 | ccnm |
| --- | --- | --- |
| 本机 Mac（fodelf 那边叫它 `xdwmbp`） | Runtime，执行账号 `bing` | 0.14.0 `fd36fb2c…` |
| fodelf（Mac） | 第 1 步当外部 MCP Host；第 2 步当 Agent，用日用 Controller（pid 52660） | 0.14.0 `fd36fb2c…` |

两步都只用临时配置：本机放在会话临时目录里，经包装脚本导出 `CCNM_CONFIG` 和 `XDG_STATE_HOME`；fodelf 上放在 `/private/tmp/ccrp/`，由那边配置里的 `[nodes.runtime] ccnm_bin` 指向本机的包装脚本（Agent 反连 Runtime 用的是 Agent 自己配置里的 `ccnm_bin`，见 [v0.13.0 记录](2026-10-08-release-0.13.0.md)第 4 节）。日用配置、`authorized_keys`、`~/.ssh/config` 一个字没动。全程没有调用模型，没有读任何凭据文件，连回去的命令只有 `true`。

## 2. 第 1 步：exec_command 连回 AI 机器

**链路**：本机驱动 → `ssh fodelf` → `ccnm mcp bridge revprobe --mode coding`（fodelf 当 Host）→ `ssh xdwmbp` → 本机临时 Runtime 配置的 `mcp-serve` → `exec_command`。临时 Runtime 配置**不写 `runtime_user`，也不写 `allow_*`**，workspace 只开 `external_mcp = "coding"`，就是默认部署的样子。驱动是一个 import `tests/mcp_client.py` 的几十行脚本，按顺序调四次 `exec_command`（`shell` 写法）：

| 探针 | 命令 | 结果 |
| --- | --- | --- |
| 直接连 | `ssh -o BatchMode=yes fodelf true` | **rc=0** |
| 谁放行的 | 同上加 `-v`，只留认证、对端软件、退出状态几行 | `remote software version Tailscale`；`Authenticated to <tailnet 地址> using "none"`；`Exit status 0` |
| 一把钥匙都不用 | `ssh -o BatchMode=yes -o PubkeyAuthentication=no -o IdentityAgent=none -o IdentitiesOnly=yes fodelf true` | **rc=0** |
| 命令环境 | `echo $SSH_AUTH_SOCK`；`launchctl getenv SSH_AUTH_SOCK` 有没有值；`id -un` | `SSH_AUTH_SOCK` 没有（ccnm 去掉了）；`launchctl getenv` 也是空；`bing` |

`initialize` 报 `0.14.0`、12 个工具。bridge 正常关闭。

**对照**：从 fodelf 连本机（普通 sshd，`OpenSSH_10.3`），带钥匙是 `using "publickey"`、rc=0；加上同样三个"不用钥匙"的参数是 `Permission denied (publickey,password,keyboard-interactive)`、rc=255。所以第三条探针能过，靠的是 fodelf 用 Tailscale SSH，不是 ssh 客户端有什么别的门路。

**关于 `launchctl getenv`**：会话前按文档推断过"macOS 上 `launchctl getenv SSH_AUTH_SOCK` 能找回 agent socket"。这台 Mac 上从 exec（经 sshd 进来）里问，是空的，这次没复现。没在别的 Mac 上试，不能推成"找不回"。不管找不找得回，私钥文件和 `~/.ssh/config` 的别名都还在，所以文档里的堵法写在 AI 机器那一侧（不接受登录），不靠藏客户端的钥匙。

## 3. 文档里给的自测命令照抄跑过

```bash
ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null fodelf true; echo $?
```

本机 zsh 里打出 `0`（同时打一行 `Warning: Permanently added … to the list of known hosts`，写进的是 `/dev/null`，known_hosts 没变）。后两个 `-o` 是为了不让"没见过这台的指纹"冒充"连不上"：新建的账号（比如 `ccrun`）known_hosts 是空的，不加时报 `Host key verification failed`、也是 255，可模型自己加上这两个参数就过去了。

## 4. 第 2 步：只从 AI 机器发起

**配置**：本机另一份临时 Runtime 配置，`ccnm init --agent ccnm-revprobe-noagent.invalid` 写出（`.invalid` 保证解析不了；本机直接 `ssh` 它是 rc=255），`workspace add ccnm` 指向一个只有 README 的临时目录。名字沿用日用的 `ccnm`，是为了让 fodelf 上 Claude Code 落在已经信任过的占位目录里，不弹信任提示、不往 `~/.claude.json` 加新条目。fodelf 上 `ccnm init --runtime xdwmbp` 写出临时 Agent 配置，再加一行 `ccnm_bin` 指向本机的包装脚本；状态目录用日用的，好让 CLI 找到日用 Controller（旧写法 `agent_node` 的会话，Controller 不重读配置，见 `session.rs` 里 `SuperviseRequest` 只在 `identity` 有值时才读 Agent 注册表）。

### 4.1 在 AI 机器上敲，都能用

| 命令（在 fodelf 上，临时 `CCNM_CONFIG`） | 结果 |
| --- | --- |
| `ccnm init --runtime xdwmbp`（项目机器那边的 `init --agent …invalid`、`workspace add` 也一样） | rc=0；`init` 不探测 SSH |
| `ccnm doctor ccnm` | "可以用了（2 项不查）"，rc=0。`反向 SSH` 正常（runtime as bing）；`Runtime 执行身份`、`admin 组`、`SSH 私钥`、`Claude 凭据` 是"注意"——共用账号的样子；`远端 MCP 握手` 12 个工具 |
| `ccnm run ccnm --detached` | rc=0，"起好了"，起于 Controller pid 52660、Aqua |
| `ccnm status ccnm`（12 秒后） | `detached  tools connected` |
| `ccnm attach ccnm`（终端里 `ssh -t`） | 接上，屏幕是 Claude Code 2.1.296 停在输入框、上下文 0%；从另一条 ssh `tmux -L ccnm detach-client` 摘下，打出 `[detached (from session ccnm-ccnm)]` |
| `ccnm ls` | `运行中 · 没有终端接着  1 分钟  通`，rc=0 |
| `ccnm stop ccnm` | `stopped ccnm-ccnm`，rc=0；fodelf 上 tmux server 没了 |
| `ccnm log ccnm` | 第一行"被停止"，rc=0 |

会话跑着时本机 `ps` 只有一个 `ccnm internal mcp-serve`，没有任何指向 fodelf 或那个 `.invalid` 名字的 ssh 客户端；停下后 `mcp-serve` 没了，临时状态目录里写锁是 `released`。这和[架构说明](../architecture.md#2-agent---runtime)第 2 种拓扑写的一致：从 Agent 发起时只把只读问题问过来，没有任何路径要 Runtime 反过来连 Agent。

### 4.2 代价：哪些用不了

在 fodelf 上：

| 命令 | 结果 |
| --- | --- |
| `ccnm ccnm --print hi` | `CCNM_E_INVALID_ARGS: --print has to be run where the projects are`，rc=34，没碰到模型 |
| `ccnm cleanup ccnm` | `CCNM_E_CONFIG: workspace 'ccnm' is not defined on this node …`，rc=10 |

在项目机器上（同一份连不回去的配置；状态目录要换短路径，见第 6 节）：

| 命令 | 结果 |
| --- | --- |
| `doctor ccnm` | `连 Agent 的 SSH` 失败（`CCNM_E_AGENT_UNREACHABLE: … Could not resolve hostname …`），10 项没查，rc=20 |
| `ccnm ccnm --detached`、`ccnm ccnm --print hi`、`status`、`stop`、`log` | `CCNM_E_AGENT_UNREACHABLE`，rc=20 |
| `attach ccnm` | 先是 ssh 的解析错误，再是"说不好会话还在不在跑"，rc=255 |
| `ls` | 表照出，多一行"问不到 … 上的会话"，rc=0 |
| `cleanup ccnm` | 预览只说"Agent 问不到：它和 Runtime 上的东西这次都没列、也不会动"，rc=0 |

`ccnm rpc` 没测：[Machine Protocol](../protocol/machine-protocol-v1.md) 写明它跑在持有项目的 Runtime Node 上，`rpc/session.rs` 也是按本机配置解析 workspace，所以这种部署下同样用不了。

合起来：只从 AI 机器发起，日常的开会话、接回、停、看状态、体检都在；**没有 `--print`、`cleanup` 和 `ccnm rpc`**。

## 5. 两种堵法和它们共同的前提

写进了[生产安全](../production-safety.md#默认配置下项目机器能连回-ai-机器)，这里只记依据：

- **只从 AI 机器发起，并让 AI 机器不再接受项目机器的登录**：第 4 节证明 ccnm 不需要这条连接。要删的是 **AI 机器** `authorized_keys` 里项目机器那把公钥：客户端那边换目录藏私钥、靠 ssh-agent，模型都可能找到（第 2 节）。
- **建专用执行账号**：命令以 `ccrun` 跑，它名下没有能登 AI 机器的钥匙；你自己的账号照旧持有那把钥匙，`--print`、`cleanup`、`rpc` 都还在。
- **共同前提**：AI 机器不能不看钥匙就放行。Tailscale SSH 这种按网络身份授权的，两种堵法都没用（第 2 节和 P7.5）。改完用第 3 节的自测确认，打出非 0 才算。

## 6. 顺带看到的（没改）

- **`ccnm init --agent` 的提示**写着"下面这条必须不用输密码就能过：ssh <别名> true"。只从 AI 机器发起时这条不需要；要不要改提示，跟着默认值的决定走。
- **doctor 的 `SSH 私钥` 行**不试连，看不出"不要钥匙也连得回去"。要它能看出来，得让执行账号真去连一次 AI 机器——这正是控制链刻意不做的事，没动。
- **项目机器一侧发起的命令**用 ssh ControlPath，状态目录放在会话临时目录里时 ControlPath 展开到 195 字节，超过 macOS 的 103，doctor 报 `CCNM_E_CONFIG: ControlPath … expands to 195 bytes`。第 4.2 节为此另用了一个短状态目录。只影响测试搭法。
- fodelf 日用 `ccnm log ccnm` 里多了一条这次的探针会话（`ad40cf91`，被停止，1 分钟）。

## 7. 用户的决定

**用户 2026-10-11 定：默认不需要 `ccrun`。** 共用账号仍是默认，快速开始仍按双向 SSH 写、`init --agent` 的提示不改；生产安全里两种堵法以"只从 AI 机器发起"为先，`ccrun` 只在要在项目机器上用 `--print`、`ccnm rpc`，或者项目机器上那个账号里还有别的不想让模型碰的东西时才建。同日四个记录提交已推送（`0180e3c..eaf8645`），随后按这个决定同步了 architecture、troubleshooting、support-matrix 各一处指向生产安全的说明。

作者自己这对机器：fodelf 上的 Tailscale SSH 让两种堵法都失效。用户同日要求关掉它、以后不走 Tailscale SSH，过程见第 9 节。

## 8. 收尾

fodelf 上 `/private/tmp/ccrp/` 和本机的短状态目录 `/private/tmp/ccrp-rt/` 已删；两边没有残留的 bridge、`mcp-serve`、tmux server。本机临时配置和驱动脚本在会话临时目录里，随会话清掉。

## 9. 关掉 fodelf 的 Tailscale SSH

用户同日说"关闭 fodelf tailscale ssh 进行测试，如果还能连接，那就关闭好了，以后就不通过 tailscale ssh 的方式了"。关掉之后要是连不上，就没法远程开回来，所以先预检、再带着自动回滚关。

### 9.1 关之前的预检

| 检查 | 结果 |
| --- | --- |
| fodelf 上的 tailscale | Homebrew 装的 tailscaled 1.102.2，root 跑；`tailscale debug prefs` 里 `"RunSSH": true` |
| fodelf 自己的 22 端口（`nc 127.0.0.1 22`） | `SSH-2.0-OpenSSH_10.2`：系统的远程登录开着 |
| 借现有连接转到 fodelf 的 sshd（`ProxyCommand="ssh fodelf nc 127.0.0.1 22"`，`HostKeyAlias` 设成 known_hosts 里记的 tailnet 地址，`StrictHostKeyChecking=yes`） | 主机钥匙**和 known_hosts 里那把一致**；认证 `Permission denied (publickey,password,keyboard-interactive)`，rc=255 |
| 为什么不认 | 本机有 `~/.ssh/fodelf`，它的公钥就在 fodelf 的 `authorized_keys` 里（只比了指纹，2 条里对上 1 条）；但本机 `~/.ssh/config` 的 `Host fodelf` 没写 `IdentityFile`，ssh 只试默认名字的钥匙。以前能连全靠 Tailscale SSH 不看钥匙 |
| 同一条预检加 `IdentityFile ~/.ssh/fodelf` | OpenSSH，主机钥匙一致，`using "publickey"`，rc=0 |

### 9.2 改了什么

- **本机 `~/.ssh/config`**：`Host fodelf` 里加一行 `IdentityFile ~/.ssh/fodelf`。原文件备份在 `~/.ssh/config.bak-2026-10-11-before-identityfile`。钥匙本身和两边的 `authorized_keys` 没动。
- **fodelf**：`tailscale set --ssh=false --accept-risk=lose-ssh`。第一次没带 `--accept-risk`，CLI 发现命令来自 Tailscale SSH 会话就拒了（`You are connected using Tailscale SSH; this action will result in your session disconnecting.` … `aborted, no changes made`），什么都没改；权限不是问题，fodelf 账号就能改。
- **回滚保险**：关的命令放在 fodelf 上一个单独的 tmux（`-L ccrp-revert`）里跑，同一个 tmux 里 5 分钟后没看到确认文件就 `tailscale set --ssh=true`。关掉后新连接成功，随即放确认文件、关掉那个 tmux。

### 9.3 关之后

| 检查 | 结果 |
| --- | --- |
| 本机新连 fodelf | `remote software version OpenSSH_10.2`，`using "publickey"`，rc=0 |
| 不带钥匙（`-o PubkeyAuthentication=no -o IdentityAgent=none`） | `Permission denied`，rc=255 |
| `ssh fodelf false` | rc=1。Tailscale SSH 下是 0（[排错手册](../troubleshooting.md#message-is-not-valid-for-protocol-1-ccnm-versions-probably-differ)"SSH 不传退出码"） |
| fodelf 的 `RunSSH`、tailnet | `false`；`tailscale status` 照常在线 |
| 日用 `ccnm doctor ccnm`（本机发起） | 可以用了（2 项不查）；`连 Agent 的 SSH`、`反向 SSH`、`Controller`、`远端 MCP 握手` 都正常 |
| 第 2 节的四条 exec 探针重跑（同一份临时配置） | 直接连 rc=0（OpenSSH、`publickey`）；不用钥匙 rc=255 `Permission denied`；命令环境同前 |
| 快速开始里写的预检命令照抄（`ssh -o BatchMode=yes -o ProxyCommand="ssh fodelf nc 127.0.0.1 22" fodelf true`） | rc=0。这次是关掉之后跑的，验的是命令写法；关之前跑的是 9.1 里加了 `HostKeyAlias` 等参数的同一条 |

**结论**：这对机器上，项目机器能不能连回 fodelf，现在只看 `~/.ssh/fodelf` 这把钥匙。默认配置的口子还在（这是用户定的默认），但两种堵法现在都管用。

**跟着变的**：以前靠 Tailscale SSH 免钥匙连 fodelf 的都不行了，比如 hpsrv 的 `bing`（以前不用钥匙就能以 `fodelf` 身份登进去）、手机上走 Tailscale SSH 的 SSH App，以及 tailnet ACL 放行的其他本地账号。要用就按[快速开始第 2 节](../getting-started.md#2-配好双向-ssh)配钥匙。快速开始原来的别名示例也少了 `IdentityFile` 这一行，已补上。

**收尾**：fodelf 上的回滚 tmux、它的日志和确认文件、`/private/tmp/ccrp/` 都已删。
