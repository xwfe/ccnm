# 快速开始

## 环境要求

### Agent Node

- macOS
- ccnm
- 官方 Claude Code，且已经登录
- 交互式会话需要 `tmux`
- 能访问 Anthropic
- 能通过 SSH 访问 Runtime Node

### Runtime Node

- macOS
- 与 Agent Node 完全相同的 ccnm build
- 真实项目 workspace
- 项目所需 toolchain（`git`、Rust、Node、Python 等）
- `search_text` 需要 `ripgrep`
- 能通过 SSH 访问 Agent Node

ccnm 不负责配置 Tailscale、VPN、Tunnel、SSH key 或 `~/.ssh/config`。它只使用已经能正常工作的 OpenSSH alias。

## 1. 先验证双向 SSH

在 Runtime Node：

```bash
ssh agent-ssh-alias true
```

在 Agent Node：

```bash
ssh runtime-ssh-alias true
```

正常日用时，这两个方向都应该能非交互执行，不应再要求手输密码。

## 2. 初始化 Runtime Node

```bash
ccnm init --agent agent-ssh-alias
cd /path/to/project
ccnm workspace add my-project
```

**两台机器各初始化一次，每次只给一个 alias。** 给哪个 flag 同时说明了这台机器是谁：在放项目的机器上给 `--agent`，在跑 Claude 的机器上给 `--runtime`。两个一起给会被拒绝。

写出来的文件长这样：

```toml
this = "runtime"          # 我是 runtime 这个 node

[nodes.runtime]           # 我自己，不需要 ssh

[nodes.agent]
ssh = "agent-ssh-alias"   # 从我这里连 agent，用这个 alias
```

`ssh` 永远是"**从读这个文件的机器出发**连那个 node 的 alias"。两台机器各写各的，因为 alias 只在定义它的那台机器的 `~/.ssh/config` 里有意义——你家里管工作机叫 `work`，工作机管你家里叫 `home`，没有一个全局名字。

Runtime Node 是 workspace root 的唯一事实来源。不要在 Agent Node 再维护一份 workspace 列表。

## 3. 初始化 Agent Node

```bash
ccnm init --runtime runtime-ssh-alias
ccnm controller install
ccnm controller status
```

这边写出来的多一行：

```toml
this = "agent"
runtime_node = "runtime"  # 我不存 workspace 列表，问这个 node

[nodes.agent]

[nodes.runtime]
ssh = "runtime-ssh-alias"
```

`runtime_node` 这行不能省。**没有它，一台刚 init 完、还没加过项目的 Runtime Node，和一台 Agent Node 的配置文件长得一模一样**——ccnm 分不出来，就会把请求转给对方，对方再转回来。

Controller 在 macOS 上通过 LaunchAgent 跑在 GUI 登录会话里。这样即使请求最初来自 SSH，官方 Claude Code 进程仍然能使用正常登录会话中的 Keychain / OAuth 上下文。

Agent Node 必须至少有人在本机 GUI 登录过一次。锁屏没关系，但只有 SSH 登录而没有 GUI 登录会话时，Controller 无法提供正确的 Claude 登录上下文。

## 4. 运行 doctor

在 Runtime Node：

```bash
ccnm doctor my-project
```

真实项目开始前，先处理所有硬失败。尤其确认：

- Runtime Node 已安装 `rg`；
- 两个 Node 上是同一个 ccnm build；
- Agent Node 上 Claude Code 已登录；
- 双向 SSH 都能正常工作；
- 项目 root 仍然存在。

`doctor` 是只读检查，不会替你创建系统账号、改 ACL 或登录 Claude。

## 5. 真实项目先配置 Runtime Service Account

对于有价值的项目，不建议长期依赖：

```toml
allow_unconfined_exec = true
```

应该在 Runtime Node 创建专用低权限账号，例如 `ccrun`，然后配置：

```toml
[nodes.runtime]
runtime_user = "ccrun"
```

详细做法见 [生产安全](production-safety.md)。系统用户、ACL 和网络策略仍然故意由人手工配置；ccnm 负责检查边界，不会静默修改主机安全模型。

## 6. 启动项目

在 Runtime Node：

```bash
ccnm my-project
```

两边配置完成后，也可以直接在 Agent Node 执行同一条命令。Agent Node 会让 Runtime Node 解析 workspace 并发起完整启动流程，然后在本地 attach 到 Agent session。

## dogfood 期间升级

两台 Node 必须部署**同一个二进制 build**。仅比较 Cargo 版本号不足以区分两个都叫 `0.2.0`、但代码不同的本地 build，因此开发阶段优先使用仓库的部署脚本：

```bash
scripts/deploy.sh <other-node-ssh-alias>
```

脚本会使用新文件 + rename 的方式替换二进制，并按当前 `ccnm controller` 接口重启 Controller。

升级完成后重新执行：

```bash
ccnm doctor <workspace>
```
