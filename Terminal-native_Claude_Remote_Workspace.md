# ccnm — Terminal-native Claude Remote Workspace

```text
Primary architecture   SSH stdio Remote Coding MCP        （正文）
Fallback architecture  SMB Hybrid Remote Workspace        （附录 A）
```

2026-09-03 起，主方案从 SMB Hybrid 切换为 SSH stdio Remote Coding MCP。切换原因见第 6 节，
Hybrid 的全部设计和已验证事实保留在附录 A，什么条件下回退也写在那里。

Phase 0 的代码（config / error / process runner / doctor / CLI skeleton）全部保留。Hybrid
Phase 1 的 SMB / identity / mount 代码从主线移除，留在 git 历史里（见附录 A.0）。

---

## 1. 产品目标

`ccnm` 解决一个非常明确的问题：

```text
工作机：
- 允许登录 Claude
- 运行 official Claude Code
- 所有 Anthropic 请求从这里出去
- 磁盘空间有限

家庭机：
- 禁止 Claude 登录
- 保存真实项目
- Node / Rust / Docker / Git 执行环境
- 大容量磁盘
```

用户只需要在家庭机 Terminal：

```bash
ccnm run xshun
```

获得：

```text
家庭机 Terminal
       │
       │ SSH TTY
       ▼
工作机
┌──────────────────────────────┐
│ official Claude Code         │
│ Claude OAuth                 │
│                              │
│ ──────────── HTTPS ─────────►│ api.anthropic.com
│                              │
│ MCP client                   │
└───────────────┬──────────────┘
                │
                │ one persistent SSH stdio
                ▼
家庭机
┌──────────────────────────────┐
│ ccnm internal mcp-serve      │
│                              │
│ workspace_info               │
│ read_file                    │
│ list_files                   │
│ search_text                  │
│ apply_patch                  │
│ exec_command                 │
│ read_output                  │
│                              │
│ real project filesystem      │
└──────────────────────────────┘
```

说白了就是：Claude 的脑子在工作机，手和眼睛在家庭机，中间只有一条 SSH。

禁止：

```text
Claude Desktop
Desktop SSH
OAuth forwarding
OAuth proxy
remote ccd-cli
家庭机 claude login
自定义 Anthropic client
HTTP 公网 MCP
Cloudflare Tunnel
FRP
```

V1 的 MCP transport 只能是：

```text
stdio over SSH
```

---

## 2. 技术选择

```text
Rust = 运行时全部核心逻辑
```

TS 不进入生产链路。原因：

```text
单 binary
启动快
JSON/path 处理安全
家庭机不需要 Node/Bun 才能运行 ccnm
工作机也不用管理额外 runtime
```

`which ccnm` 只对应一个 Rust executable。家庭机运行 ccnm 本身不需要 Node / Bun / Tauri / WebView。

MCP server 用现成的 Rust MCP SDK，不手写半套协议（第 25 节）。SDK 需要 tokio 就用 tokio：
之前"V1 不用 tokio"是 Hybrid 架构下的决定，不是教条。

---

## 3. 一个 binary，三个角色

不发布 `ccnm-client` / `ccnm-server` / `ccnm-runner`，只有一个 `ccnm`，按 subcommand 扮演角色：

```text
家庭机 launcher       ccnm run / doctor / attach / status / stop        用户直接调用
工作机 controller     ccnm internal hello / probe / work-run            家庭机 ssh 过去调用
家庭机 MCP runtime    ccnm internal hello / mcp-serve                   工作机 ssh 回来调用
```

`internal` 子命令在 `--help` 里隐藏，参数只有一个 `--payload <TOKEN>`（第 8 节）。

三个角色是同一份 binary，部署、版本兼容和协议升级都只有一个东西要管。

---

## 4. CLI 设计

### 检查

```bash
ccnm doctor xshun
```

Primary MCP 模式全部落地后，一切正常时的输出：

```text
ccnm doctor: xshun

Config                  OK     /Users/fodelf/.config/ccnm/config.toml
Workspace config        OK     backend=mcp-ssh work_host=work (ssh xdwmbp), runtime_host=home (ssh_from_work fodelf)
Home workspace          OK     /Users/fodelf/Projects/xshun
Home ccnm               OK     0.1.0 at /Users/fodelf/.local/bin/ccnm
Tailscale               OK     direct via 203.0.113.7:41641
Work SSH                OK     bing@xdwmbp
Work ccnm               OK     0.1.0
Claude Code             OK     2.1.259 (/opt/homebrew/bin/claude)
Claude authentication   OK     me@example.com via claude.ai (max)
Reverse SSH             OK     fodelf as fodelf, ccnm 0.1.0
Remote MCP handshake    OK     initialize, tools/list (1 tool, 412 B), workspace_info x1 in 190 ms
Workspace root          OK     git repo
Workspace policy        OK     7 tools, schema 9.8 KiB
Project instructions    WARN   CLAUDE.md is 19 KiB, over the 16 KiB instructions budget; not injected
Native tools disabled   OK     Read Edit Write Grep Glob Bash
Runtime identity        SKIP   not implemented until phase 5
Network isolation       SKIP   not implemented until phase 5
Terminal session        SKIP   not implemented until phase 6

NOT READY (0 failed, 3 not checked)
```

每行四种状态：

```text
OK      查过，没问题
WARN    查过，有值得看一眼的地方；不阻塞 READY
SKIP    没查成：前置项失败，或这个版本还没实现。阻塞 READY
FAIL    查过，坏了；带 CCNM_E_* 错误码和修复提示。阻塞 READY
```

exit code 规则：

```text
有 FAIL            → 第一个 FAIL 行的错误码
没 FAIL、有 SKIP   → CCNM_E_NOT_READY (3)
只有 OK / WARN     → 0
```

"没查成"和"查出坏了"必须是两个不同的码：`ccnm run` 的 preflight 看到 3 知道是环境没验证完，
看到 21 知道是反向 SSH 坏了。骨架阶段的 doctor 因此不可能误报 READY，也不会把"还没实现"冒充成
某个具体故障。

从 "Work SSH" 往下的信息来自**一次** `ssh work ccnm internal probe` 往返：工作机跑
`claude --version` 和 `claude auth status`，反向 ssh 回家庭机跑 `ccnm internal hello`，再起一次
短暂的 MCP handshake（第 27 节），打包成一个 JSON 回来。工作机不需要 config 文件，它要的参数都在
请求里。

不再检查（Hybrid 专有）：SMB mount、SMB coherence、mount identity、write barrier。

### doctor 永远 read-only

这是 invariant。doctor 永远不能：

```text
安装 binary
创建用户
修改 ssh config
创建 project files
启动长期 MCP server
修改 permissions
```

允许短暂启动一个 probe MCP server 做 handshake，但 doctor 结束前必须把它收掉（关 stdin、等退出、
超时就 kill）。

原因：doctor 会被 `ccnm run` 的 preflight、cron、CI 反复调用。一旦"检查顺手改了状态"，同一条命令
跑两次结果就不一样，出了问题也分不清是环境本来就坏还是 doctor 弄坏的。

具体到 SSH：doctor 探活时带 `-o ControlMaster=no`。OpenSSH 文档写明这个值只复用已有 master，
socket 不存在就普通连接，不会留下一个后台 master 进程。

### 正常使用（Phase 6 才实现）

```bash
ccnm run xshun        # preflight，工作机起 official Claude，当前 TTY attach
ccnm attach xshun     # 重新 attach 工作机 tmux，不新建第二个 MCP server
ccnm status xshun
ccnm stop xshun
```

Primary 模式没有 `ccnm mount` / `ccnm unmount` / `ccnm workspace init` / `ccnm maintenance`：
没有 mount，就没有需要维护的第二份视图。`git switch`、`pnpm install`、`cargo fmt` 就在家庭机上
通过 `exec_command` 正常跑。

---

## 5. 配置文件

家庭机是配置 source of truth：

```text
~/.config/ccnm/config.toml
```

```toml
version = 1

[hosts.work]
# 家庭机 ~/.ssh/config 里指向工作机的 alias
ssh = "work"
# 可选。不设就复用工作机默认的 ~/.claude，见第 21 节。
# claude_config_dir = "/optional/custom/path"
# 可选。工作机上 ccnm 的绝对路径；不设就用 ~/.local/bin/ccnm，见第 7 节。
# ccnm_bin = "/Users/work/.local/bin/ccnm"

[hosts.home]
# 工作机 ~/.ssh/config 里指向家庭机的 alias
ssh_from_work = "ccnm-home"
# ccnm_bin = "/Users/ccrun/.local/bin/ccnm"

[workspaces.xshun]
backend = "mcp-ssh"          # 默认值，可省略

work_host = "work"
runtime_host = "home"        # 默认值 "home"，可省略

# runtime host 上项目的真实路径。它不需要、也不应该在工作机存在。
root = "/Users/fodelf/Projects/xshun"

claude_permission_mode = "acceptEdits"
```

校验规则（strict：未知字段直接报错，不静默忽略）：

```text
version                      必须是 1
work_host                    必须指向一个有 ssh 的 host
runtime_host                 必须指向一个有 ssh_from_work 的 host
root                         绝对路径，不含 . / ..
ccnm_bin / claude_config_dir 设了就必须是绝对路径
```

MCP 模式**不再要求两台机器 root 相同**。这是这次架构切换最直接的收益之一：工作机上根本没有这个
目录。

如果以后要用 Hybrid fallback：

```toml
[workspaces.legacy]
backend = "hybrid-smb"
work_host = "work"
runtime_host = "home"
root = "/Users/Shared/cc-workspaces/legacy"
runtime_root = "/Users/Shared/cc-runtime/legacy"
share = "legacy"
mount_mode = "coherence"
```

`share` / `mount_mode` / `runtime_root` 只在 `backend = "hybrid-smb"` 下合法且必填；
`hosts.<runtime_host>.smb_user` 也只有 hybrid 会用到。在 `mcp-ssh` workspace 里写这些字段是
CCNM_E_CONFIG。当前 binary 只解析 hybrid-smb 配置，不实现它（doctor 会 FAIL 说明）。

config 里不存：

```text
Claude OAuth
SSH private key
SMB password
```

secret 继续由 macOS Keychain、OpenSSH、系统 SMB credential 负责。

---

## 6. 核心 invariant

```text
Anthropic Control Plane = 工作机
Workspace/Data Plane    = 家庭机
Transport               = SSH stdio
```

必须始终满足：

```text
工作机：
  official Claude Code
  Claude OAuth
  api.anthropic.com

家庭机：
  ccnm MCP runtime
  project
  git
  rust/node/bun/docker
  无 Claude login
  无 Claude OAuth
```

### 为什么从 Hybrid 切过来

```text
1. 所有 Anthropic 请求仍然只从工作机官方 Claude Code 发出（这条没变）
2. 家庭机不登录 Claude、不持有 OAuth（这条没变）
3. 文件 / 搜索 / patch / git / 构建全部在家庭机同一个 filesystem namespace
4. 删除 SMB + SSH 双通路的一致性问题
5. 删除 mount / cache / barrier / single-writer 那一大套复杂度
6. SSH 只建立一条 persistent stdio transport，不是每个 tool call 新建 SSH
7. search / exec / output retention 在数据所在地完成，具备控制 token 的条件
```

Hybrid 里最重的部分（第 5 条）全是为了让"工作机透过 SMB 看到的文件"和"家庭机本地文件"一致。
MCP 模式下工作机根本不看文件，这一整层问题不存在。

### Primary 不再依赖

```text
SMB
相同绝对路径
mount_smbfs
SMB cache
source write plane
hash barrier
```

### ccnm 管编排，不管网络

> **ccnm owns orchestration, not networking.**
>
> Network connectivity is an external capability by default. Built-in connectivity
> integrations such as Tailscale are optional adapters and conveniences, never hard
> dependencies.
>
> Hosts are addressed through transport endpoints such as OpenSSH aliases. ccnm must
> work with any underlying network — LAN, public IP, VPN, Tailscale, WireGuard,
> ZeroTier, FRP, Cloudflare Access, ProxyJump, or user-defined transports — without
> requiring the core to understand how reachability is established.

说白了：**ccnm 不管理网络，它只消费"已经可用的连接能力"。**

ccnm 真正需要的只有两件事：

```text
工作机能执行    ssh ccnm-home
家庭机能执行    ssh work
```

这两个 hostname 最终经过什么网络到达，ccnm 不该关心，也不该有办法关心。

#### 四层

```text
┌─────────────────────────────────────────────┐
│               Claude Layer                  │
│  工作机上的 official Claude Code            │
└─────────────────────┬───────────────────────┘
                      │ MCP
┌─────────────────────▼───────────────────────┐
│                CCNM Runtime                 │
│  workspace / read / search / patch / exec   │
│  policy / session / output retention        │
└─────────────────────┬───────────────────────┘
                      │
┌─────────────────────▼───────────────────────┐
│             Transport Adapter               │
│  ssh / local / 将来的自定义 transport       │
└─────────────────────┬───────────────────────┘
                      │
┌─────────────────────▼───────────────────────┐
│          Connectivity / Infrastructure      │
│  Tailscale / LAN / WireGuard / ZeroTier     │
│  FRP / Cloudflare / 公网 IP / VPN / JumpHost│
└─────────────────────────────────────────────┘
```

前三层属于 ccnm。**最后一层默认不属于 ccnm**，是用户已经准备好的基础设施。

#### 落到代码上的三条硬规矩

```text
1. 依赖方向单向向下：Runtime 只认识 Transport Adapter，不认识任何具体网络方案的名字。
   Transport Adapter 只认识 "一个 ssh endpoint"，不认识它背后是什么。
2. V1 核心代码里不出现 tailscale 这个依赖。同理不出现 wireguard / zerotier / frp /
   cloudflared。任何一个都只能是 optional adapter，装不装、用不用，核心逻辑都不变。
3. 主机一律用 transport endpoint 寻址（OpenSSH alias）。不在 config 里存 IP、
   MagicDNS 名、tailnet、隧道 URL——这些是 ~/.ssh/config 的事（第 7 节）。
```

#### 怎么验证这条原则没被破坏

用户把底层从 Tailscale 换成下面任何一种，ccnm 核心代码应该一行都不用改：

```text
Tailscale + SSH        FRP + SSH             Cloudflare Access + SSH
WireGuard + SSH        ZeroTier + SSH        公网 SSH
LAN SSH                ProxyJump
```

换的只是用户 `~/.ssh/config` 里 `Host ccnm-home` 那几行怎么写。ccnm 看到的永远是
`ssh ccnm-home`。

#### 当前的偏差（已知，未修）

```text
crates/ccnm-core/src/tailscale.rs        201 行，只读 `tailscale status --json`
crates/ccnm-core/src/doctor.rs:48,307,577  doctor 直接 import 它，有一行 Tailscale 检查
crates/ccnm-cli/src/main.rs:15,118        CLI 负责找 tailscale binary
```

这段代码本身是无害的：只读、从不阻塞、找不到 `tailscale` 就报 OK 跳过，SSH 通不通由 SSH
自己说了算，它只解释延迟（direct 还是 DERP 中转）。但**名字出现在 `ccnm-core` 里就是方向错了** ——
Runtime 层不该认识第四层的任何一个具体方案。

不为此停工。等下次动 doctor / SSH transport 时一起收拾，目标形态是：核心只有一个
"transport 诊断"接口，Tailscale 变成一个可选 adapter，编译进不进去都不影响 `ccnm run`。
在那之前，新代码不许再引用 `tailscale` 模块。

---

## 7. SSH 身份与 ccnm binary

仍然只用：

```text
/usr/bin/ssh
~/.ssh/config
known_hosts
ssh-agent
Tailscale
ProxyJump
```

不引入 Rust SSH client。

分工是 invariant：

```text
用户 ~/.ssh/config    决定 Host、HostName、User、IdentityFile、ProxyJump、Tailscale 地址
ccnm                  只在命令行追加 ControlMaster / ControlPath / BatchMode / 安全覆盖项
```

用户自己维护两个 alias，ccnm 只读：

```sshconfig
# 家庭机 ~/.ssh/config
Host work
    HostName <工作机 tailscale 名>
    User bing

# 工作机 ~/.ssh/config
Host ccnm-home
    HostName <家庭机 tailscale 名>
    User ccrun                     # Phase 5 之前先用普通用户
    IdentityFile ~/.ssh/ccnm_ed25519
```

alias 名字随便取，config.toml 里写对就行。

ccnm 每次调用 ssh 时追加（用 `Command::args()`，不写进任何 config 文件）：

```text
-o BatchMode=yes
-o ConnectTimeout=10
-o ControlMaster=auto|no
-o ControlPath=~/.local/state/ccnm/ssh/%C
-o ControlPersist=10m
-o ServerAliveInterval=15
-o ServerAliveCountMax=3
-o SendEnv=-ANTHROPIC_*
-o SendEnv=-CLAUDE_*
```

OpenSSH 规定命令行选项优先于 `~/.ssh/config`（每个参数取第一个出现的值），所以这些追加项一定生效。

只用 OpenSSH 自带的能力：

```text
ssh -G alias         打印最终解析出的配置，不建连接。doctor 用它显示实际会用的 HostName / User
ssh -O check alias   问 master 是否活着
ssh -O exit alias    让 master 退出，ccnm stop 用
```

### remote binary 在哪

开发阶段两边固定装在：

```text
~/.local/bin/ccnm
```

非交互 ssh 走登录 shell 的 `-c`，zsh 这时只读 `~/.zshenv`，不读 `~/.zshrc`，所以 PATH 靠不住。ccnm
在远端命令里直接写路径 `~/.local/bin/ccnm`（`~` 由远端 shell 展开，所有 POSIX shell 和 fish 都
支持），不写裸的 `ccnm`。远端找不到会退出 127，doctor 报：

```text
Work ccnm   FAIL   CCNM_E_VERSION: ~/.local/bin/ccnm not found on work
                   install the same ccnm build there, or set hosts.work.ccnm_bin
```

路径不对就在 config 里写绝对路径 `ccnm_bin`。

现在不做：

```text
自动 scp
自动升级
sudo install
deployment manager
```

人工保证两边同版本；版本不一致 doctor 报 CCNM_E_VERSION。

### 一个会撞的坑

macOS 上 unix socket 路径最长 104 字节（`sys/un.h` 里 `sun_path[104]`）。ControlPath 超过就报
`ControlPath too long`，看着像 ssh 坏了，实际是路径长。`%C` 展开后是 40 个十六进制字符，所以
`~/.local/state/ccnm/ssh/` 这个前缀在 HOME 正常长度时够用。doctor 会算展开后的长度，超了直接
FAIL 并说明。

---

## 8. 版本化 internal control protocol

```text
CCNM_PROTOCOL_VERSION = 1
```

家庭机 ↔ 工作机之间所有控制请求：

```text
serde_json
  ↓
base64url no-pad
  ↓
单个 argv token
```

例如：

```text
ccnm internal hello --payload <TOKEN>
ccnm internal probe --payload <TOKEN>
ccnm internal mcp-serve --payload <TOKEN>
ccnm internal work-run --payload <TOKEN>
```

不通过 SSH 拼任意 shell command。远端命令行上只出现 `[A-Za-z0-9_-./=:@+,~]`，任何登录 shell
都不会误解析；请求内容里的引号、`$`、换行、`|`、`&&` 都躲在 base64 里。

每个请求和响应都带 `protocol`。不一致就是两边 ccnm 版本不同，报 CCNM_E_VERSION，不去猜。

远端进程的两个输出流：

```text
stdout   协议输出（JSON 响应）/ MCP 输出（JSON-RPC）
stderr   diagnostic（tracing 日志、panic）
```

绝不能混。一行 debug 日志混进 stdout，对面的 JSON parser 就炸，而且看起来像"版本不兼容"。

---

## 9. MCP 不套在 control protocol 里

两层协议必须分开：

```text
CCNM control protocol    launcher / hello / probe / session setup
MCP protocol             Claude ↔ coding runtime
```

MCP 建立后：

```text
Claude Code
  ↓ stdio MCP JSON-RPC
ssh stdin/stdout
  ↓
ccnm internal mcp-serve
```

MCP JSON-RPC 直接穿 SSH stdio。不要：

```text
MCP JSON → 再包一层 base64 CCNM JSON → 再 SSH
```

`--payload` 只用于**启动** mcp-serve 时告诉它 workspace / root / session；启动之后 stdin/stdout
就完全交给 MCP。

---

## 10. `ccnm run xshun` 的启动流程（Phase 3 / 6）

家庭机：

```bash
ccnm run xshun
```

```text
1. load config

2. local preflight
   - workspace root 存在
   - 本机 ccnm 版本
   - work SSH config 可解析

3. SSH → work

4. work-side ccnm
   - hello / version
   - claude --version
   - claude auth status

5. work 创建：
   ~/.local/state/ccnm/sessions/<session-id>/

6. 动态生成：
   mcp.json
   settings.json
   session metadata

7. work 启动 official claude

8. 家庭机当前 TTY attach 到 work Claude
```

不复制项目源码到工作机。

---

## 11. MCP config 的形态

工作机上 `ccnm` 为每个 session 生成 `mcp.json`：

```json
{
  "mcpServers": {
    "ccnm": {
      "type": "stdio",
      "command": "/usr/bin/ssh",
      "args": [
        "-T",
        "-o", "BatchMode=yes",
        "-o", "ClearAllForwardings=yes",
        "-o", "ServerAliveInterval=15",
        "-o", "ServerAliveCountMax=3",
        "-o", "SendEnv=-ANTHROPIC_*",
        "-o", "SendEnv=-CLAUDE_*",
        "ccnm-home",
        "~/.local/bin/ccnm",
        "internal",
        "mcp-serve",
        "--payload",
        "<BASE64URL>"
      ]
    }
  }
}
```

payload：

```json
{
  "protocol": 1,
  "workspace": "xshun",
  "root": "/Users/fodelf/Projects/xshun",
  "session": "…",
  "policy": "coding"
}
```

生命周期：

```text
Claude Code 启动一次 MCP server
  ↓
ssh process 建立一次
  ↓
整个 MCP session 共用它
```

绝不能出现：

```text
每个 read_file  → spawn ssh
每个 search     → spawn ssh
```

这一条必须有测试证明（第 27 节的 100 次调用测试）。

---

## 12. OpenSSH multiplexing 与 MCP transport 分开理解

MCP stdio server 自己已经持有一条长连接：

```text
work ssh process ↔ home ccnm internal mcp-serve
```

所以 MCP tool call 本身不需要 ControlMaster。

ControlMaster 只服务：

```text
home launcher → work         doctor / hello / probe / run
work → home 的额外探测       probe 里的 hello
```

保留 `ControlMaster=auto` + `ControlPersist=10m` 给这些短命令，但架构不依赖"每个工具调用复用
ControlMaster"。正常 MCP runtime 应该只有：

```text
one long-lived SSH process
```

---

## 13. Claude 原生工具在 MCP 模式必须关闭

Full MCP 模式不允许模型同时拥有：

```text
native Read / Edit / Write / Grep / Glob / Bash
```

否则它会读写工作机自己的磁盘，而工作机上没有项目。

用 Claude Code 2.1.259 实测存在的 flag（2026-09-03 `claude --help`）：

```text
--disallowed-tools <tools...>   逗号或空格分隔的工具名
--mcp-config <configs...>       JSON 文件或 JSON 字符串
--strict-mcp-config             只用 --mcp-config 里的 MCP server，忽略其他配置
--tools <tools...>              "" 禁掉全部内置工具，"default" 全开，或列名字
--settings <file-or-json>
--setting-sources <sources>     user,project,local
--permission-mode <mode>        acceptEdits | auto | bypassPermissions | manual | dontAsk | plan
```

Phase 3 的启动命令形态：

```bash
claude \
  --mcp-config "$CCNM_SESSION/mcp.json" \
  --strict-mcp-config \
  --disallowed-tools Read Edit Write Grep Glob Bash \
  --settings "$CCNM_SESSION/settings.json" \
  --setting-sources user,project,local \
  --permission-mode acceptEdits
```

`--tools ""` 比 `--disallowed-tools` 更彻底，但它同时会禁掉 WebFetch、Agent 之类。Phase 3 两个都
实测，按结果定，不猜。

不要只靠 prompt。argv 用 `Command::args()` 逐个传，不拼 shell 字符串。

---

## 14. 工具集：7 个，不是 20 个

这是 token 和 tool-selection 的核心原则。每多一个工具，`tools/list` 的 schema 就常驻 context，
模型选错工具的概率也上升。

Phase 2 第一版只做：

```text
1. workspace_info
2. read_file
3. list_files
4. search_text
5. apply_patch
6. exec_command
7. read_output
```

Git 暂时走：

```text
exec_command("git status --short")
exec_command("git diff -- src/x.rs")
```

benchmark 之后再决定要不要加 `git_status` / `git_diff`（第 30 节）。

第一版不加：

```text
history / planning / task
OAuth
image
port forward
SFTP
tunnel
multi-workspace management
GPT Actions
```

Claude Code 已经有自己的 session / context workflow，ccnm 不做第二套 agent harness。

---

## 15. Tool schema 要刻意压缩

原则：

> move computation to data, move answers back, not files.

所有 path 都是 **workspace-relative**（`src/main.rs`），模型永远看不到 `/Users/fodelf/…`。

### workspace_info

```text
输入   无
输出   workspace 名、git 是否存在（以及 root 在 repo 里的相对位置）、platform
```

不返回几十个 env。

### read_file

已实现（`crates/ccnm-core/src/mcp/read.rs`）。

```text
输入   path, start_line?, end_line?, max_lines?, max_bytes?
默认   max_lines = 200（上限 2000）, max_bytes = 32 KiB（上限 64 KiB）
       超出上限是 clamp 不是报错——"最多给我 N 条"里的 N 大只是偏好，不是错误
输出   content[0].text  = 右对齐行号 + `→` + 该行，末尾一行 footer 说明下一步
       structuredContent = 元信息，不含文件正文（第 16 节：正文不能算两遍）
```

绝不能默认整文件无限读，也不能先把整个文件读进内存再截断——按行流式读，撞到第一个上限就停，
所以读一个 2 GB 日志的开头和读 2 KB 文件一样便宜。coding-tools-mcp 是先整读再截（研究记录 m.7）。

structuredContent 字段：

```text
path start_line end_line lines bytes truncated
truncated_by      max_lines | max_bytes；调用方自己写的 end_line 或读到文件尾都不算 truncated
next_start_line   还有后文时给出；读到文件尾才没有
total_lines       只有读到文件尾才知道。没读完还去数行要再扫一遍全文，正是这个工具要省的开销
file_bytes line_ending(lf|crlf|mixed|none) final_newline notes[]
```

真实使用里会撞、而且都有测试锁住的坑：

```text
workspace 里的 fifo/设备    先 stat 再 open。open 一个没有 writer 的 fifo 会永久阻塞，
                            单线程 runtime 上会把整个 session 后面每一次调用一起冻住
minified 的 300 KB 单行     按字符边界切（直接切字节会 panic，而带重音或 emoji 的文件天天遇到）；
                            next_start_line 指向下一行而不是本行，否则调用方会原地死循环
二进制文件                  前 8 KiB 探 NUL 直接拒，不拿 context 换乱码
latin-1 等非 UTF-8 文本     lossy 解码并在 notes 里说明，不拒
CRLF / BOM / 末尾无换行      显示时规范化并如实上报，apply_patch 之后要靠这些信息
start_line 超过文件末尾      明确回 "文件只有 N 行"，不给一个空结果让模型瞎猜
start_line 深到离谱          扫描超过 64 MiB 就拒，让它去用 search_text
```

schema 里 `start_line` 等字段必须写 `minimum: 1`。不写的话 schemars 会从 `u32` 推出
`minimum: 0`，等于 ccnm 自己的 tools/list 告诉模型 `start_line: 0` 合法，然后代码再拒绝它。

### list_files

已实现（`crates/ccnm-core/src/mcp/list.rs`）。

```text
输入   path?, glob?, max_entries?, include_hidden?
默认   max_entries = 200（上限 1000）, include_hidden = false
输出   workspace-relative 路径，一行一条，目录带结尾 /；末尾一行 footer
```

不返回 mtime / inode / permission / owner。目标是帮模型导航，不是实现 `ls -la`。
路径是 workspace-relative 而不是相对 `path`，因为模型下一步就是把它原样贴进 `read_file`。

**glob 决定形态，不另设 recursive 开关：**

```text
不给 glob   列 path 的直接子项（目录带 /）
给 glob     在 path 下递归匹配，任意深度；glob 相对 path 解释
```

少一个参数，而且这就是人本来的思路：要么打开一个目录，要么去找东西。

**列什么，由 git 说了算。** 这是这个工具有没有用的分水岭——对真实项目做一次朴素递归遍历，
200 条预算在碰到第一个源文件之前就耗在 `node_modules` 和 `target` 里了。

```text
git workspace    git ls-files --cached --others --exclude-standard
                              --directory --no-empty-directory -z -- <scope>
                 = 项目自己对"什么算数"的定义：已跟踪 + 新增，减去 .gitignore /
                   全局 ignore / .git/info/exclude 排除掉的；未跟踪目录折叠成一条
非 git workspace  有界遍历 + 一张短的跳过表（node_modules target dist build venv vendor）
```

`source` 字段如实说是哪一种。模型看不到 `target/` 时，得能分清"git 忽略了它"和"ccnm 猜的"。
coding-tools-mcp 是写死 13 项、没有开关（研究记录 c 节）。

**遍历里两个问题必须分开答，合成一个就是 bug：**

```text
这个条目"是"什么   决定怎么列。指向目录的 symlink 就该列成目录——列成文件会把模型
                   引到 read_file，而 read_file 会拒绝它
要不要"进去"       由链接本身决定，symlink 一律不跟进。这既是遍历跑出 workspace 的
                   途径，也是它撞上死循环的途径
```

glob 语法只支持 `*` / `**` / `?` / `{a,b}`（`crates/ccnm-core/src/mcp/glob.rs`）。字符类
`[a-z]` 是**报错**而不是当字面量——当字面量的后果是匹配不到任何东西，模型据此认定文件不存在。
匹配用 DP 表不用递归：`a/**/**/**/**/b` 是让回溯式 glob 指数爆炸的经典写法，而这个 runtime
接受模型发来的任意 pattern。

### search_text

已实现（`crates/ccnm-core/src/mcp/search.rs`）。最重要的 token 优化工具之一。

```text
输入   query, path?, glob?, regex?, case_sensitive?, context_lines?, max_results?
默认   max_results = 50（上限 200）, context_lines = 2（上限 10）,
       regex = false（即 literal）, case_sensitive = true
实现   家庭机本地 rg；达到 max_results 或字节预算立即杀掉 rg
输出   按文件分组，`行号:` 是命中、`行号-` 是上下文，跨段之间一行 `--`
```

`max_bytes` **不做成参数**，内部固定 32 KiB。它是 context window 的属性，不是这次提问的属性；
做成参数，能调高的调用方一定会调高。单行另有 512 字节上限，否则一个 minified bundle
一条命中就能吃掉整个预算。

**搜索在文件所在地完成**，只有命中回来。不把文件搬到工作机再搜——这正是 runtime 要住在家庭机的理由。

**rg 负责扫描，ccnm 负责全部约束。** 不自己写文本扫描器：匹配语义、编码探测、ignore 文件优先级、
multiline 这些已经存在，自己写只会更慢而且错得不一样。但**约束一条都不继承**，rg 今天的默认值安全
不是依赖它的理由：

```text
--no-config   环境里的 RIPGREP_CONFIG_PATH 不能改变 ccnm 搜什么、怎么搜
--no-follow   symlink 是搜索跑出 workspace 的途径
--no-hidden   dotfile 不搜，.git 也在其中
-g !.git      再说一遍 .git。将来万一有个开关把 hidden 打开，不能连这条一起打开
cwd = root    rg 拿到的是相对 scope，所以它打印的路径是相对的，家庭机的绝对路径到不了模型
```

然后**还要再查一遍 rg 的输出**：路径是绝对的、带 `..` 的、在 `.git/` 下的，一律丢弃。
rg 是快速扫描器，不是安全边界。错误文本里的 workspace 路径也先替换掉再往外送。

**两个上限限制的是"做多少事"，不是"回多少事"**：`stream_lines` 边读 rg 的 JSON 边判断，
撞上限就杀掉 rg。在 monorepo 里搜 `e` 的代价是 50 条命中，不是全扫一遍再截断。
命中数满了不立刻停——最后一条命中的尾部 context 还要收完，否则答案停在命中行上，读起来像文件到头了。

四个容易写错、各有测试钉住的地方：

```text
rg exit 1     = 没匹配，是答案不是失败。>= 2 才是错误
regex=false   必须真 literal（--fixed-strings）。查 `b.c` 不能匹配到 `bXc`
query 位置    必须在 `--` 之后。否则查 `--ignore-case` 会被当成 flag
长行截断      按字符边界切，直接切字节会 panic
```

`structuredContent` 里放 path / line / column，**不放命中文本**——那是贵的部分，而且它就在
`content` 里紧挨着自己的 `path:line` 前缀。

家庭机没装 rg 时报 `CCNM_E_NOT_READY` 并说清装法：没坏，也不能用。

### apply_patch

已实现（`crates/ccnm-core/src/mcp/patch.rs`）。**源码修改的唯一入口。**

不提供 `write_file(full_content)`：整文件写入的代价随文件大小走而不是随改动走，正好和这个架构的
目的相反，而且会把模型上次看过之后发生的任何改动一起吞掉。

```text
输入   files[]（op / path / to? / version? / content? / edits[]）, dry_run?
op     add / update / delete / move
edits  { old, new, replace_all? }，按顺序作用在上一条的结果上
输出   每个文件一行摘要 + 新 version；不回传任何文件内容
```

**patch 是精确替换的列表，不是 unified diff。** diff 需要一个解析器，而且实践中还得靠模糊匹配才
扛得住模型写错的 hunk 头。精确 `old` → `new` 没有歧义，代价随改动大小走，而且白送一个 diff 要费劲
才能保证的性质：**被替换区间之外的字节完全不变**，所以 BOM、CRLF、末尾无换行全都原样保留，不需要
任何一行代码专门去"保持格式"。`old` 出现多次时报错而不是猜，除非显式给 `replace_all`。

**stale baseline**：改动已存在的文件必须带上 `read_file` 返回的 `version`；文件在这之间被写过就拒绝，
什么都不做。**不给 version 也拒绝**——那意味着模型压根没读过这个文件。只匹配 `old` 是不够的：它只
证明被改的那一段没变，不证明模型对文件其余部分的理解还成立。

`version` 是 size + mtime，**不是内容 hash**，这是故意的：`read_file` 是流式的，能在不读完 2 GB 文件
的前提下回答前 200 行，做 hash 就把这个性质扔了。size 和 mtime 来自它本来就要做的 `stat`。代价：
从备份恢复、或者带时间戳复制过来的文件会被误判为"变了"——这是安全的方向。

**三个阶段，每个阶段的理由：**

```text
plan     解析路径、校验 version、读原文、算出新内容。磁盘一个字节都没碰，
         所以任何问题都是整次调用失败且什么都没写
stage    把每个新内容、以及每个原文，写到目标文件同目录的 temp。仍然不可见。
         磁盘满是在这里失败，而不是在 commit 中途
commit   只有 rename 和 unlink。同目录 rename 是原子的：读者看到的要么是旧文件
         要么是新文件，不会是半写的，也不存在文件短暂消失的窗口
```

commit 中途失败（stage 成功之后还失败，意味着文件系统正在我们脚下出问题）就把备份 rename 回去并
报告失败。**绝对不能报成功**——"你的一部分文件被改了"必须可见，所以连回滚也失败时报得更响，
并列出每一个涉及的文件。

顺带保住的两件事：文件权限（patch 一个脚本不能让它失去可执行位）；`add` 时创建的目录在 patch
失败后会被删掉。

写侧路径策略是读侧那套再加三条（`resolve_write`，第 17 节）。

### exec_command

已实现（`crates/ccnm-core/src/mcp/exec.rs`）。第二个 token 成本核心。

```text
输入   cmd（数组）, cwd?, timeout_ms?, preview_bytes?
默认   timeout 120 s（上限 600 s）, preview 4 KiB（上限 16 KiB）
输出   status / exit_code / 头尾 preview / output_ref
```

`max_output_bytes` 没做成参数：它约束的是 ccnm 在**用户机器上**留下多少东西，那不是调用方的决定。
内部固定单流 64 MiB，超了照样跑完、照样报 exit code，只是留存的副本到此为止并说明。

#### 这不是 sandbox，代码里也没假装是

第 18 节。路径校验保护 `read_file` 和 `apply_patch`，在这里**什么都保护不了**——命令能去它所运行的
那个用户能去的任何地方：`cat ~/.ssh/id_ed25519`、`curl -d @secrets ...`、`rm -rf ~`。

**这一阶段故意不做 deny list。** 一张禁止程序名的表，用 `env claude`、绝对路径、或者一个 wrapper
脚本就能绕过；它真正的效果是让工具**看起来**被管着而其实没有。假的安全感比没有更糟，设计文档
原话就是"command parser 不是 sandbox"。

真正让它安全的是 Phase 5 而不是 Phase 2：家庭机上一个专用 Unix 用户（`ccrun`），只能碰这个项目，
没有 sudo、没有 ssh key、没有 Claude credential、没有浏览器 profile，加上 filesystem ACL 和第 19 节的
网络策略。在那之前，`exec_command` 的可信度**完全等于 runtime 所用账号的可信度**，而设计文档已经把
dedicated runtime identity 写成真实日用前的硬门禁。

这一阶段唯一强制的是核心 invariant：**任何 `ANTHROPIC_*` / `CLAUDE_*` 变量都不传给子进程**。家庭机
不持有 Claude credential，也不能通过 ccnm 跑的某条命令学到一个。其余环境照常继承——要用 PATH 找
cargo 的命令得能找到。

#### argv，不是 shell

`cmd` 是数组，没有 `sh -c`，所以 ccnm 里没有任何需要转义的地方，审计行就是真正跑的东西。
想用 shell 的常见理由都已经被别的东西覆盖了：

```text
cargo test 2>&1 | tail -50   输出本来就有上限且能分页，直接跑 cargo test
cd sub && make               cwd 参数
RUST_LOG=debug cargo test    ["env", "RUST_LOG=debug", "cargo", "test"]
ls *.rs                      list_files
grep -r x .                  search_text
```

#### 一个真 bug：kill 不杀进程组

`Child::kill` 只发给一个进程。`sh -c 'echo x; sleep 30'` 里 sleep 是 sh 的子进程、还握着 stdout 管道，
所以只杀 leader 的话 drain 线程会一直读到 sleep 结束——**一个不会超时的超时**。现在两个 spawn 点都把
子进程放进自己的进程组，杀的是整组（用 `kill(1)`，因为这个 crate 禁 unsafe）。`stream_lines` 有同样的
毛病，所以 `search_text` 的提前终止之前也是坏的。研究记录里早就写了 coding-tools-mcp 有这个问题。

### read_output

已实现（`crates/ccnm-core/src/mcp/output.rs`）。第七个、也是最后一个工具。

```text
输入   output_ref, stream?（stdout|stderr）, offset?, limit?
默认   limit 16 KiB（上限 32 KiB）
输出   这一页的内容 + next_offset / total_bytes / eof
```

不能每次把前面的 output 重新发一遍。offset 是**字节偏移且稳定**——引用产生时文件已经写完了，一个 run
的输出永远不变，所以一小时后 offset 4096 还是同一个地方。这就是分页便宜的原因：不用维护游标，
不重发任何东西。

`output_ref` **按形状匹配，不当路径清洗**：`..`、斜杠、长度不对，在被拼到任何东西上之前就失败了，
所以根本没有"路径穿越"这件事要做对。而且只在**本 session 的目录**里解析——它是一个 session 内部的
引用，不是这台机器上的句柄。

每一页都回退到字符边界再切，下一个 offset 也就落在边界上。不这么做的话，任何带非 ASCII 的输出
每个分页接缝都会多一个替换字符。limit 小到装不下一个字符时直接报错，而不是原地不前进地死循环。

---

## 16. `structuredContent` 同样 bounded

不要假设 "structuredContent 不算 token"。ccnm 不依赖 client 是否把它完整送给模型。

```text
content            concise
structuredContent  同样 bounded
large payload      本地保留 + output_ref
```

"content 只放摘要，structuredContent 塞 2 MB stdout" 只是把问题藏起来。

---

## 17. Workspace root security

MCP server 启动时 canonicalize 配置的 root。之后 read / list / search / patch 全部只接受
workspace-relative path，拒绝：

```text
绝对路径（含 C:\ 这种 Windows 写法）
../                     哪怕是 a/../a 这种自己抵消掉的也拒
~ 开头
symlink escape（canonicalize 之后不在 root 下）
NUL、反斜杠
```

实现在 `crates/ccnm-core/src/mcp/path.rs`，只有这一处；每个文件工具都从这里过，不各写一份近似的。

两条容易写错的细节：

```text
先判越界再判存在   否则 `up/does-not-exist` 的回答会泄漏 workspace 外面有什么文件。
                   现在两种情况都回 CCNM_E_POLICY
错误码分两类       CCNM_E_POLICY      = 越界，别重试，换参数也没用
                   CCNM_E_INVALID_ARGS = 参数本身不对（不存在、是目录、行号是 0），改了再来
                   混成一个码，模型会因为一个笔误就放弃，或者对着墙一直撞
```

`.git`：普通 file tool 禁止修改。Git 操作只能通过 `exec_command` / 未来的 git tool。
**读**目前是放行的（`read_file .git/config` 能读到）。留一句在这里：`.git/config` 里可能有
`https://user:token@github.com/...`，模型读到就等于把它带回工作机的 context。Phase 5 的 policy
要不要把 `.git` 读也关掉，是个待定项，不是遗漏。

---

## 18. `exec_command` 是真正的安全边界难点

path validation 保护不了：

```bash
cat ~/.ssh/id_ed25519
curl …
rm -rf …
```

因为 shell command 本身可以跳出 workspace。所以 Production V1 不允许只靠 command regex。

最终 runtime 用家庭机专用 Unix 用户：

```text
ccrun
```

它：

```text
可以完整读写指定项目
可以运行项目 toolchain
无 sudo
无家庭用户 SSH key
无 Claude credential
无浏览器 credential
无个人 home secrets
```

需要的话用 ACL / group 只给它目标项目和 runtime directory 权限。

Phase 1A / 1B / 2 的 fixture benchmark 可以先用当前用户。但：

> 在真实项目日用之前，dedicated runtime identity 是硬门禁。

Production exec policy（Phase 5）区分 safe / ask / deny，至少默认 deny：

```text
sudo / su
ssh / scp
rsync 到任意外部 host
直接启动 claude
读取 ~/.ssh
读取系统 credential
```

但文档必须明确：**command parser 不是 sandbox**。真正的生产安全依赖 dedicated OS identity、
filesystem ACL、network policy。

---

## 19. 家庭机网络边界

硬约束：

```text
api.anthropic.com 只能工作机访问
```

MCP server 自身绝不包含 Anthropic SDK / OAuth / model API。

启动 runtime 时清理环境：

```text
ANTHROPIC_*
CLAUDE_*
CLAUDE_CODE_OAUTH_TOKEN
CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST
```

SSH 每次调用追加 `-o SendEnv=-ANTHROPIC_*` `-o SendEnv=-CLAUDE_*`。OpenSSH 支持用 `-` 前缀清掉
用户 config 里已有的 SendEnv 模式，所以即使用户全局配了 `SendEnv *`，这两类变量也不会跟着 ssh
出去。家庭机 sshd 的 AcceptEnv 默认为空，是第三道保险。

但必须说清楚：

> `exec_command` 是通用 shell，仅靠 ccnm tool policy 不能证明任意子进程永远不会主动访问 Anthropic。

如果这条网络出口约束是绝对合规边界，Production gate 要求 ccrun 账户 / 执行沙箱没有公网 egress，
或至少由 OS / network policy 阻断 Anthropic。不要把静态 command deny 写成"网络安全边界"。

Phase 1A / 1B fixture benchmark 不需要联网。

---

## 20. Full MCP 特有问题：项目 CLAUDE.md

切 MCP 后：

```text
Claude process cwd = 工作机
真实 repo         = 家庭机
```

所以不能再假设工作机 Claude 会自动加载家庭机的：

```text
CLAUDE.md
.claude/rules/
.claude/skills/
```

这是 Phase 3 gate。

第一顺位验证 `MCP initialize response.instructions` 能否让 Claude Code 稳定得到：

```text
CCNM remote workspace instructions
+
家庭机 root CLAUDE.md
```

先只验证 root `CLAUDE.md`，不一次复制整个 Claude config model。

instructions 必须 bounded：最大 8–16 KiB。过大明确 WARN（doctor "Project instructions" 行），
不静默塞进 context。

### 如果 instructions 不够

再实现 work-side shadow workspace：

```text
~/.local/state/ccnm/shadow/<workspace-id>/
```

只允许同步 `CLAUDE.md` 和 `.claude/rules/`，以后再研究 `.claude/skills/`。绝不复制 `src/`、
`.git/`、`node_modules/`、`target/`。每次 `ccnm run` 前重新生成。它只是 project metadata
projection，不是 project mirror。

---

## 21. Claude config namespace：默认复用，可选隔离

V1 默认**不设置** `CLAUDE_CONFIG_DIR`。工作机上的 ccnm 直接复用当前已经登录的 Claude Code：

```text
不制造第二份 OAuth / token 生命周期
用户现有 ~/.claude/CLAUDE.md、skills、user settings 照常生效
hooks / settings 的隔离由 --settings <session file> 完成，不需要换目录
```

### 可选：自定义目录

```toml
[hosts.work]
claude_config_dir = "/some/path"
```

设置后，所有 Claude 相关 preflight 和最终启动统一带 `CLAUDE_CONFIG_DIR=<该路径>`；doctor 在同样
的环境下执行 `claude auth status`，未登录只报告一行并给出人工登录命令：

```text
Claude authentication   FAIL   Claude is not authenticated in configured CLAUDE_CONFIG_DIR
                               run on work: CLAUDE_CONFIG_DIR=/some/path claude auth login
```

### 为什么自定义目录必须自己登录

官方 authentication 文档明确：设置了 `CLAUDE_CONFIG_DIR` 后，`.credentials.json` 放在该目录下，
macOS Keychain 条目也按该目录 key。2026-09-03 在 2.1.259 实测：空目录下 `claude auth status`
返回 `loggedIn: false`。换目录等于换账号环境，登录不会跟过来。所以 V1 推荐保持默认目录。

### ccnm 对认证的唯一动作

ccnm 绝不执行或自动触发 `claude auth login`，绝不复制 credentials。它只检查
`claude auth status --json`（登录 exit 0，未登录 exit 1）。

### 一个会撞的坑：SSH 会话读不到 Keychain

2026-09-03 在真实工作机（macOS，Claude Code 2.1.258）实测：GUI 里已登录，`~/.claude/.credentials.json`
里只有 MCP 插件的 OAuth，Claude 自己的登录在 login Keychain 的 `Claude Code-credentials` 条目里。
通过 `ssh work claude auth status --json` 得到 `loggedIn: false`，同一会话里
`security find-generic-password -s "Claude Code-credentials" -w` 退出码 36
（errSecInteractionNotAllowed：Keychain 锁着，或者会话没有 UI 可以弹授权框）。

所以 doctor 的 "Claude authentication FAIL" 要读成"**从 ssh 会话看**没登录"。这对 Phase 3 是硬问题：
`ccnm run` 通过 SSH 在工作机起 Claude，Claude 同样读不到 Keychain。候选做法（Phase 3 实测决定）：

```text
1. 工作机保持 GUI 登录且 Keychain 解锁，看 Claude 进程（不是 security CLI）能否免弹窗读取
2. 在工作机 GUI 会话里跑一个常驻 tmux server，ccnm run 只 attach，Claude 继承 GUI 会话的 security session
3. CLAUDE_CONFIG_DIR 指向一个用文件存凭据的目录（要单独 claude auth login 一次）
```

ccnm 不会去 `security unlock-keychain`：那需要用户密码，违反"不碰认证"。

### ccnm 不修改项目 `.claude/settings.json`

不能让安装 ccnm 修改 repository 或团队 `.claude/settings.json`。session 级配置动态生成到
`~/.local/state/ccnm/sessions/<id>/settings.json`，用 `--settings` 传入。

---

## 22. Hook 在新架构里不是核心

Hybrid 需要 PreToolUse Bash rewrite、PostToolUse Write tracking、SessionStart、Barrier。

Full MCP 后 Bash / Read / Edit / Write 已禁用，这些 Hook 全部从核心 runtime 移除。

如果最后 project context 需要 SessionStart 注入，只用一个非常小的 SessionStart hook
（`additionalContext` 上限 10,000 字符，超过会被换成文件路径加预览）。不要重新建立 Hook routing
architecture。

doctor 仍然扫描会参与 session 的 settings（`~/.claude/settings.json`、workspace `.claude/settings*.json`），
发现会改写 Bash `tool_input` 或 deny/allow 文件工具的 hook 至少 WARN。不要自动改写或合并用户已有
hooks，发现冲突就报出来。

---

## 23. tmux 与 MCP lifecycle（Phase 6）

```text
家庭机 shell
  ↓ ccnm run
SSH TTY → work
  ↓
tmux ccnm-xshun
  ↓
official claude
  ↓ SSH stdio MCP
home ccnm internal mcp-serve
```

家庭机 terminal → work tmux 断开时 Claude 仍然活着，那么 work Claude → home MCP SSH 也应该继续
活着。所以 MCP transport 的生命周期绑定 **Claude process**，而不是家庭机 outer SSH TTY。

`ccnm attach` 只重新 attach work tmux，不重新创建第二个 MCP server。

网络断开时 work Claude/tmux 与 home MCP lifecycle 的处理要明确写出来，但不在 spike 阶段先做漂亮 UX。

---

## 24. Error code 必须从第一天稳定

名字和进程 exit code 一起固定：

```text
名字                       exit   含义
CCNM_E_INTERNAL             1     bug 或意外的 OS 错误，不是给用户分类用的
CCNM_E_NOT_READY            3     doctor 没有 FAIL 但有 SKIP（没验证完），或者功能还没实现
CCNM_E_CONFIG              10     config.toml 缺失、解析失败或校验不过
CCNM_E_VERSION             11     两台机器 ccnm 版本 / protocol 不一致，或 Claude Code 太旧
CCNM_E_AUTH                12     工作机 Claude 未登录
CCNM_E_WORK_UNREACHABLE    20     家庭机 SSH 不到工作机
CCNM_E_HOME_UNREACHABLE    21     工作机 SSH 不到家庭机 runtime
CCNM_E_MOUNT               22     （Hybrid）SMB share / mount 缺失或不可用
CCNM_E_WRONG_WORKSPACE     30     workspace root 不存在、不是目录，或 canonicalize 后不对
CCNM_E_COHERENCE           31     （Hybrid）hash 不一致，命令没有执行
CCNM_E_STALE_EPOCH         32     （Hybrid）session epoch 过期
CCNM_E_POLICY              33     runtime 不允许这个操作（路径越界、.git、deny 的命令）
CCNM_E_INVALID_ARGS        34     工具参数本身不能用：行号是 0、区间反了、path 指向不存在
                                  的东西/目录/二进制文件。和 33 分开是因为模型的反应不同：
                                  33 是"别试了"，34 是"改了参数再来"
CCNM_E_DEPENDENCY          35     家庭机缺一个 runtime 依赖的外部程序（比如 search_text 要的
                                  rg）。和 3 分开：3 是 ccnm 自己没做完，35 是那台机器上要跑
                                  一条安装命令，ccnm 再怎么写也修不好
```

`CCNM_E_STALE_EPOCH`(32) 原本是 Hybrid 的 session epoch 过期，现在也用于 `apply_patch` 的
stale baseline。含义一致：**你手上那份基准过期了，先重新读**。

exit 0 是成功，2 留给 clap 的用法错误。加新码可以，改名或改号不行：另一台机器上可能还跑着旧版
ccnm。Hybrid 专有的码保留编号，不复用。

Claude 收到的错误第一行永远是 `CCNM_E_X:`，后面是人话。

---

## 25. Repository 结构与依赖

Phase 1A / 1B 不拆十个 crate：

```text
crates/
├── ccnm-cli          clap 入口
└── ccnm-core
    └── src/
        ├── config.rs
        ├── error.rs
        ├── paths.rs
        ├── process.rs
        ├── claude.rs
        ├── tailscale.rs 待搬走：核心不该认识具体网络方案（第 6 节"ccnm 管编排，不管网络"）
        ├── doctor.rs
        ├── protocol/    payload 编码、hello、probe 请求响应
        ├── ssh/         ssh 命令行构造、双向探测（Transport Adapter 层）
        └── mcp/         MCP server / probe client
            ├── path.rs  workspace 路径策略，所有文件工具共用（第 17 节）
            ├── glob.rs  glob 匹配（list_files / 将来的 search_text）
            ├── read.rs  read_file
            ├── list.rs  list_files
            ├── search.rs search_text（rg）
            ├── patch.rs apply_patch（三阶段：plan / stage / commit）
            ├── exec.rs  exec_command（argv，无 shell；输出落盘）
            ├── output.rs read_output（字节偏移分页）
            └── server.rs
```

等 Minimal Coding Runtime 边界稳定后再拆 `ccnm-mcp`。不要为了架构图漂亮提前拆。

依赖：

```text
现有        clap, serde, serde_json, toml, tracing, tracing-subscriber, base64, uuid
MCP         rmcp 3.2（官方 Rust MCP SDK；features server, macros, transport-io, client, transport-child-process）
            tokio 1（rt, io-std, process, time；current_thread runtime，只在 crates/ccnm-core/src/mcp 里）
```

选 rmcp 的依据（2026-09-03 实测，细节在 `docs/research/mcp-spike-2026-09-03.md`）：stdio / initialize.instructions /
tools/list / tools/call / structuredContent / cancellation / stdin EOF 退出全部现成；server + client 共 78 个 crate，
没有 hyper / reqwest / axum；main 仍是同步的，async 只在 `mcp/` 一个目录里。

coding-tools-mcp 的研究结论在 `docs/research/coding-tools-mcp.md`：没有 headless stdio 入口，只参考 contract，
不复用代码。

TS 只能出现在 `tests/`、`tools/`、fixture 生成器里，`ccnm run` 永远不要求 node / bun 存在。

---

## 26. 新的 Phase 划分

### Phase 0 — skeleton（已完成）

Cargo workspace、config parser、error model、logging、process abstraction。

### Phase 0.1 — doctor 语义（已完成）

CCNM_E_NOT_READY，OK / WARN / SKIP / FAIL 四态。

### Phase 1A — Architecture viability spike

目标不是完成产品，只回答：

> SSH stdio Remote Coding MCP 是否值得取代 Hybrid？

对 `lengsukq/coding-tools-mcp` 做代码研究，输出 `docs/research/coding-tools-mcp.md`，至少记录：

```text
read semantics
search result caps
patch semantics
exec output retention
git semantics
stdio 是否有现成 headless entry
Tauri coupling
可复用模块
license / provenance
```

不因为它"看起来能用"就整个引入 dependency（第 28 节）。

### Phase 1B — Persistent SSH stdio

只实现：

```text
MCP initialize
tools/list
workspace_info
```

然后证明 Claude/work → one SSH → home MCP server 可以持续多次 JSON-RPC request。硬测试见第 27 节。
此阶段还不读真实项目。

### Phase 2 — Minimal Coding Runtime

按顺序实现 read_file → list_files → search_text → apply_patch → exec_command → read_output。
每完成一个工具：

```text
schema test
path-policy test
size-limit test
error-semantic test
real fixture integration test
```

进度：

```text
read_file      已完成（2026-09-03）。workspace 里放了一个指向家庭机
               ~/.ssh/id_ed25519 的 symlink，被 CCNM_E_POLICY 拦下。
list_files     已完成（2026-09-03）。真机对着 ccnm 仓库本身跑：git 模式下
               target/ 一次都没出现过；glob、截断、越界、非目录、不支持的语法
               各自给出正确的码。
search_text    已完成（2026-09-03）。真机验证：.git/config 搜不到、literal 的 `.`
               不当通配、查 `--ignore-case` 不被当 flag、坏 regex 报 INVALID_ARGS
               且不带路径、0 命中不是错误。
apply_patch    已完成（2026-09-03）。真机验证：read → patch → read 立刻看到新内容且
               version 对得上；重放同一个 version 报 STALE_EPOCH 且不写；不给 version
               被拒；多文件第二个失败时第一个也没被写；CRLF+BOM+末尾无换行原样保留；
               `..` / 绝对路径 / `.git` / 指向 ssh key 的 symlink / move 出去，五种越界
               全部 CCNM_E_POLICY；dry_run 什么都不写；add/move/delete 各自正确。
exec_command   已完成（2026-09-03）。真机验证：cwd 走同一套策略；非零退出是结果不是
               错误；`env` 的输出里没有任何 ANTHROPIC_/CLAUDE_；`echo hi | rm -rf /`
               是一个不存在的程序名而不是两条命令；重定向符号只是普通参数；
               `sleep 30 &` 的超时 1.5 s 就回来了（修进程组之前是 30 s）。
read_output    已完成（2026-09-03）。真机验证：128890 B 含中文的输出分 4 页取回，
               逐字节拼回原样，首行/末行/中间行都对，没有分页接缝上的坏字符；
               stderr 是独立的流；`../../../etc/passwd` 按形状被拒。
```

**Phase 2 的 7 个工具到此全部完成。**`tools/list` 共 8270 B（预算 16 KiB），最大的是
apply_patch 2487 B。

真机数据（工作机 xdwmbp 起一条 ssh 到家庭机，同一会话内 50 次调用）：

```text
                            p50      p95      max     response
workspace_info（纯 RTT 基线） 26.6     175.0    423.8   288 B
read_file Cargo.toml         31.9      60.4    192.5   1640 B
search 10 命中               52.4     195.8    205.2   3579 B
search 50 命中               54.4     211.8    472.8   6755 B
search 0 命中                65.2     210.3    229.7   265 B
search 在 'e' 上提前终止      50.2     200.2    208.8   5184 B
initialize                   305–385 ms
tools/list                   4 个工具 3735 B（预算 16 KiB）
```

**p95 的抖动是链路，不是工具。** 什么都不干的 `workspace_info` 自己就是 p95 175 ms / max 424 ms。
减掉基线，search 只比 RTT 多花约 25 ms。本地纯耗时对得上：rg 自己 13.7 ms，走完 ccnm 15.6 ms，
ccnm 只加 2 ms。

两个值得看的对照：`0 命中` 反而比 `10 命中` 慢（65 vs 52 ms），因为没有命中就没得提前停，rg 要
扫完；在 `e` 上提前终止只要 50 ms 且本地只花 7.9 ms，比全扫还快——这就是"限制的是做多少事"的证据。

`list_files` 本地纯耗时 12.4 ms，其中 `git ls-files` 自己占 9 ms。顺带发现并修掉了
`process.rs` 里 5 ms 固定轮询的问题：那个常量的理由是"跟 SSH 往返比可以忽略"，在工具开始跑
本地命令之后不成立了，改成 200 µs 起的退避轮询后 20.5 ms → 12.4 ms。`search_text`（rg）和
`exec_command` 本来会继承同一笔开销。

测试的判据不是"有测试"，是"改坏了会挂"。已做的变异验证共 15 处，每一处都有测试红：

```text
read_file/path   fifo 类型检查、containment、字符边界切割、二进制探测、CRLF 识别、
                 BOM 剥离、next_start_line 前进、max_lines 上限、`..` 拒绝
list_files/glob  遍历跟随 symlink、strip_dir 半段匹配、hidden 过滤、`**` 匹配零段、
                 字符类静默放行、git 忽略规则失效
search_text      rg 输出的路径不再复查、literal 悄悄变 regex、exit 1 当成失败、
                 字节预算去掉、query 不放在 `--` 之后、`-g !.git` 去掉、错误文本不脱敏
apply_patch      过期 version 放行、缺 version 放行、歧义 edit 直接改第一处、
                 stage 失败不清理 temp、`.git` 可写、可以写穿 symlink、权限不保留、
                 CRLF 不翻译、commit 失败不回滚
exec/output      env 不剥离、只剥 ANTHROPIC_ 漏掉 CLAUDE_、kill 不到进程组、
                 cwd 策略算了不用、preview 不截断、session id 不清洗、旧 run 不清理、
                 output_ref 不校验、分页接缝切坏字符、limit 过小死循环、offset 越界放行
```

累计 42 处，全部有测试红。

其中 "stage 失败不清理 temp" 和 "commit 失败不回滚" 第一轮**没被抓到**——当时所有失败都发生在
plan 阶段，磁盘还没被碰过。补了两个测试：把目标目录 chmod 555，第二个文件的 temp 写不进去就在
stage 失败；而 move 不写 temp，所以 move 到只读目录能过 stage、卡在 commit，那是唯一能走到回滚
分支的路径。

**在真机上抓到、测试没抓到的一个 bug**：`git ls-files --directory` 会把整个未跟踪的目录折叠成
一条 `src/`，而 `src/` 不是 `src` **里面**的路径，所以对一个 `git init` 之后还没 commit 的仓库
列 `src` 会返回"空"。单测 fixture 全都 commit 过，所以一路绿灯。已去掉该 flag 并补了一个只
`git init` 的 fixture。教训：fixture 要覆盖仓库的**各个生命周期阶段**，不只是"正常状态"。

一个诚实的说明：`-g !.git` 那条只有 argv 测试红，行为测试没红——因为 `--no-hidden` 单独已经
挡住了 `.git`。`.git` 有三层防御（`--no-hidden`、`-g !.git`、输出复查），其中第一层和第三层有
行为测试。第二层"看不出差别"正是它存在的意义。

`apply_patch` 是最后一个文件工具。不为了快速 Demo 先写 `write_file` 再承诺以后换。真正能修改真实
项目之前，apply_patch 测试至少覆盖：

```text
add / update / delete / move
CRLF/LF、UTF-8、no final newline
stale baseline、partial failure、multi-file failure
symlink escape
.git reject
```

任何 partial write 都不允许。

### Phase 3 — Claude Integration

工作机正式生成 `mcp.json` / `settings.json`，启动 official Claude Code，关闭 Read / Edit / Write /
Grep / Glob / Bash。验证 Claude 能完成：理解项目 → search → read → patch → exec test → read output。
同时解决 root CLAUDE.md project context（第 20 节）。

### Phase 4 — Benchmark

决定是否正式放弃 Hybrid 的门禁，见第 27 节。

### Phase 5 — Production Safety

benchmark 通过才进入。建立 ccrun，验证项目可读写、toolchain 可运行、无 sudo、无 Claude credential、
无个人 SSH private key、无浏览器 credential。项目需要 Git credential 就单独设计最低权限 credential，
不共享整个 Keychain / SSH agent。exec policy 见第 18 节。

### Phase 6 — Terminal UX

`ccnm run / attach / doctor / status / stop`，tmux，断线处理（第 23 节）。

### Phase 7 — Tool Parity

只有真实日用证明需要才加，优先顺序：

```text
git_status / git_diff
write_stdin / kill_session
view_image
```

不加 history / planning / task。

---

## 27. Benchmark 与 gate

### 1B 硬测试：one persistent SSH

```text
连续调用 workspace_info 100 次

home 上只存在一个对应 sshd session / ccnm mcp-serve process
work 不产生 100 个 ssh process
```

记录 connect cold latency、warm MCP call p50/p95/max、RTT。

### Benchmark fixture

```text
tests/fixtures/remote-coding-project
```

工作机、家庭机各放一份相同的小型 repo。不拿随时变化的真实项目做基准。同一 Claude model、同一
prompt、新 session、相同 permission settings、相同 repo revision。

### Micro benchmark

```text
workspace_info
read 4 KiB
read 32 KiB
list 100 files
search 10 hits
search 50 hits
apply small patch
exec true
exec small test
read_output 16 KiB
```

分别测 home-local stdio 和 SSH stdio，分离 runtime cost 和 network cost。记录 p50 / p95 / max /
request bytes / response bytes。

### Task benchmark

固定任务：找到指定函数 → 搜索调用位置 → 阅读 2–4 个相关文件 → 修改一个明确 bug → 运行测试 →
检查变更。

比较 A（工作机 local fixture + native Claude tools）和 B（家庭机 fixture + ccnm Remote MCP），
至少 5 次，成本允许 10 次。记录 wall clock、tool call count、input/output/cache tokens、MCP
request/response bytes、最大单次 tool result、失败/重试次数。

### Token 数据不要自己猜

先验证当前 Claude CLI `-p --output-format json / stream-json` 实际是否暴露 usage / input tokens /
output tokens / cache fields，按 2.1.259 实际输出实现 parser。拿不到就写 `unavailable`，不用
`bytes / 4` 冒充 token。可以另外记录 schema bytes / tool result bytes 作为工程指标，但标明不是 token。

### Tool schema budget

把 `tools/list` 完整 serialize 后记录 schema bytes。目标：7 个 core tools 总 schema 尽量 <= 16 KiB。
超了先缩 description / schema，不要为了"解释更完整"写几百字 tool description。

### Token acceptance gate

```text
Remote MCP total token usage <= native baseline + 15%
```

`<= baseline` 说明优化成功；`+0–15%` 可接受，再判断稳定性收益是否值得；`> +15%` 不能 promote，
先查 schema 过大 / read 默认太多 / search 返回太多 / exec output 太多 / 错误导致重复调用 / 模型
不会选工具，优化后重跑。

### Latency acceptance gate

不拿 RTT 当唯一判断。目标是：

```text
remote overhead ≈ 一个 SSH stdio round trip + serialization
```

一个 read_file 产生多次 SSH 往返属于架构 bug。UX 门禁：普通 read / search / tool call 不应出现明显
> 250 ms 的额外 transport delay，有就先定位。

### Consistency gate

循环 `apply_patch → 立即 exec_command 读取/编译` 至少 100 次，预期 100% 看见最新内容。这里不应该
存在 SMB cache mismatch；有 mismatch 就是 ccnm runtime 自己的 bug。

### Git 专用工具是否值得加

`exec_command("git diff")` 经常返回很多无关输出，加 `git_status` / `git_diff`（bounded、
path-filtered、structured）才有意义。普通 exec 已经够好就不为了 API 完整度加工具。

---

## 28. coding-tools-mcp 的使用原则

定义成：

```text
architecture reference
runtime contract reference
benchmark baseline
```

不是 ccnm dependency。

Phase 1A 完成前不做 git subtree / submodule / copy src-tauri / 添加 Tauri / 添加 Node。

如果它已有 headless stdio MCP server，可以临时作为 benchmark baseline，但不集成、不 vendor、不变成
runtime dependency。如果没有方便的 headless stdio，不要为了运行它把 Tauri / Desktop 搬进 ccnm，
直接继续做最小 ccnm MCP spike。

### 复用其 Rust 代码的规则

第一优先：只参考 contract，自主实现。

如果直接抽取 / 复制实现，必须先提交 `docs/third-party/coding-tools-mcp.md` 记录 repository、
commit、license、copied/derived modules、modifications，遵守其 license，保留需要的 LICENSE /
NOTICE / attribution。不先 copy 后补 provenance，不悄悄 copy。

---

## 29. V1 明确不做

```text
❌ GUI
❌ Desktop integration
❌ Anthropic API client
❌ OAuth handling
❌ Rust SSH implementation
❌ HTTP / 公网 MCP transport
❌ multi-host orchestration
❌ port forwarding manager
❌ SFTP abstraction
❌ PTY remote process manager
❌ 第二套 agent harness（history / planning / task）
❌ Windows
❌ Linux first-class support
```

先针对 macOS home + macOS work + OpenSSH + official Claude Code 把一个场景做透。

开发和验收时两台机器之间用的是 Tailscale，但那只是**当前这条链路碰巧由谁提供**，不是 V1 的一部分。
换成公网 SSH、WireGuard、FRP 或 ProxyJump，ccnm 应该一行都不用改（第 6 节）。

---

## 30. V1 验收标准

### Auth

```text
家庭机无 Claude login
家庭机无 Claude OAuth
Anthropic 请求从工作机发出
```

### Workspace

```text
所有文件读写在家庭机 filesystem
工作机没有项目副本
apply_patch 无 partial write
consistency gate 100/100
```

### Execution

```text
cargo test / pnpm test / git diff / rg / docker 都在家庭机跑
长输出留在家庭机，模型只拿 preview + output_ref
```

### Protection

```text
path escape denied
.git write denied
native Read/Edit/Write/Grep/Glob/Bash 不存在于 session
dedicated runtime identity（Phase 5）
```

### Benchmark

```text
token <= native + 15%
transport overhead ≈ 一个 RTT
```

### UX

最终日常命令不超过 `ccnm run / attach / doctor / status / stop`。

---

## 31. 项目定位

`ccnm` 不是 "Claude SSH wrapper"，而是：

> **A terminal-native remote workspace runtime for Claude Code.**

它不碰模型认证。它只负责 workspace、execution、policy、session、transport。

即使以后 Anthropic 官方推出真正的 `claude --ssh home`，ccnm 的 doctor、policy、execution
isolation、runtime management 仍然有价值。

---
---

# 附录 A：Fallback Architecture — SMB Hybrid Remote Workspace

这一部分是 2026-09-03 之前的主方案，原样保留，作为 fallback。里面所有"实测"、"核实"的事实
仍然有效，Phase 1 的代码在 git 历史里。

## A.0 现状与回退条件

代码：commit `1a7d064`（phase 1: two-way ssh, smb mount, workspace identity, real doctor）
包含 `smb.rs`、`identity.rs`、`work.rs` 的 mount/unmount、`home.rs`、`runner.rs`，以及对应的
doctor 行。之后的主线把它们移除了；要回退就从那个 commit 捡回来。

什么时候回到 Hybrid：

```text
1. Phase 4 token gate 失败：优化 schema / 默认值之后 Remote MCP 仍然 > native + 15%
2. Latency gate 失败：单个 tool call 的 transport overhead 无法压到一个 RTT 量级
3. Claude Code 在没有 native Read/Edit/Write 时明显退化，且 instructions / shadow workspace
   救不回项目上下文
4. apply_patch 在真实项目上达不到"零 partial write"门禁
```

回退不是推翻：config 已经有 `backend = "hybrid-smb"` 的位置，error code 保留了 MOUNT /
COHERENCE / STALE_EPOCH 的编号。

## A.1 Hybrid 的目标形态

```text
家庭机 Terminal
      │
      │ SSH TTY
      ▼
工作机 official Claude Code
      │
      ├──── HTTPS ────► api.anthropic.com
      │
      ├──── SMB ──────► 家庭机 source
      │
      └──── SSH ──────► 家庭机 execution
```

工作机 controller 内部命令：

```bash
ccnm work probe      # doctor 的一次性探测，只读
ccnm work mount
ccnm work unmount
ccnm work start
ccnm hook session-start
ccnm hook pre-tool
ccnm hook post-tool
ccnm exec
ccnm fs
ccnm barrier
```

家庭机 restricted runner：

```bash
ccnm runner exec
ccnm runner verify
ccnm runner health
```

用户命令多出：

```bash
ccnm mount xshun            # 在工作机挂 SMB
ccnm workspace init xshun   # 在源码 root 写 .ccnm-workspace-id
ccnm unmount xshun
ccnm maintenance xshun      # git switch / pull / install / fmt / codemod
ccnm maintenance --finish xshun
```

Hybrid 的 doctor 表：

```text
Config                  OK     /Users/me/.config/ccnm/config.toml
Workspace config        OK     work_host=work (ssh work), runner_host=home_runner (ssh_from_work ccnm-home)
Home workspace          OK     /Users/Shared/cc-workspaces/xshun
Workspace identity      OK     550e8400-e29b-41d4-a716-446655440000
SMB share               OK     xshun -> /Users/Shared/cc-workspaces/xshun
Tailscale               OK     direct via 203.0.113.7:41641
Work SSH                OK     me@workmac
Work ccnm               OK     0.1.0
Work SMB mount          OK     mounted, SERVER_NAME=home
Work identity view      OK     matches
Reverse SSH             OK     ccnm-home as ccrun
Home runner             OK     ccrun runs ccnm 0.1.0, root and runtime_root visible
Runner identity view    OK     matches
Claude Code             OK     2.1.259 (/usr/local/bin/claude)
Claude authentication   OK     me@example.com via claude.ai (max)
Consistency test        SKIP   not implemented until phase 2
Execution barrier       SKIP   not implemented until phase 5
```

## A.2 配置

```toml
[hosts.home_runner]
# 工作机 ~/.ssh/config 里指向家庭机的 alias。它解析出的 HostName 同时用作 SMB server 地址，
# 所以 ssh 和 SMB 永远指向同一台机器。
ssh_from_work = "ccnm-home"
# 工作机挂 SMB 时用的账号（家庭机上拥有 share 的账号，不是 ccrun）。密码在工作机 Keychain。
smb_user = "fodelf"

[workspaces.xshun]
backend = "hybrid-smb"
work_host = "work"
runtime_host = "home_runner"
root = "/Users/Shared/cc-workspaces/xshun"
runtime_root = "/Users/Shared/cc-runtime/xshun"
share = "xshun"
mount_mode = "coherence"   # 挂载参数含义见 A.12
claude_permission_mode = "acceptEdits"
```

`runtime_root` 不能和 `root` 重叠，否则 runner 拿到 source 写权限，single-writer 就没了。

## A.3 路径统一

两台机器上 `/Users/Shared/cc-workspaces/xshun` 必须代表同一个项目：家庭机是 real local
filesystem，工作机是 SMB mount 到家庭机。Claude 看到的 `…/src/main.rs` 和家庭机 SSH runner 看到
的是同一个路径。ccnm 不实现路径翻译，发现两边 root 不同就 doctor FAIL。

## A.4 Source Plane 与 Execution Plane

```text
Source Plane      Claude native Read / Edit / Write → SMB → 家庭机 source
Execution Plane   Claude native Bash → ccnm PreToolUse → ccnm exec → persistent OpenSSH
                  → ccnm runner exec → 家庭机（rg fd git cargo pnpm bun node docker tests build）
```

## A.5 禁掉 native Grep / Glob

它们会扫描 SMB。搜索统一走 `rg` / `fd` / `git grep`，通过 Bash 路由到家庭机本地 SSD。
`--disallowed-tools Grep Glob`，2.1.259 实测有这个参数。

## A.6 Hook：SessionStart / PreToolUse router / PostToolUse tracking

SessionStart 通过 `additionalContext` 告诉 Claude，只有这几行：

```text
CCNM remote workspace active.
Source edits use the mounted workspace.
Bash executes on the home runner.
Do not mutate source from Bash.
Source-mutating git/formatter/install commands require CCNM maintenance mode.
```

保持很短：hook 输出上限 10,000 字符，超过会被换成文件路径加预览。

PreToolUse 收到 Bash 的 `tool_input.command` 后分类：

```text
LOCAL_SAFE     极少，第一版只有纯 cd（它要改工作机侧 cwd，远程执行会让 cwd 分叉）
REMOTE         绝大多数：cargo test / pnpm test / rg / git status / git diff / node / bun / docker
               改写为 ccnm exec <opaque-session-payload>（PreToolUse.updatedInput 可以替换整个 tool input）
DENY           source mutation（cargo fmt / prettier --write / eslint --fix / git checkout|switch|
               reset|restore|pull / codemod）和 run_in_background
CCNM_INTERNAL  以 ccnm fs 开头的命令，在工作机执行，不 SSH
```

payload 用 JSON → base64url，避免 quotes / heredoc / `$` / `|` / `&&` / newline 的二次 shell
escaping（这条设计原样进了 Primary 的第 8 节）。

`ccnm exec` 在工作机验证 session 和 epoch、读取 pending writes、建立 consistency barrier，然后
`/usr/bin/ssh -T ccnm-home ccnm runner exec --payload XXX`。

PostToolUse 在 Edit / Write 成功后读取 `tool_input.file_path`，写入 pending source set。

doctor 必须扫描会参与 session 的全部 settings（`~/.claude/settings.json`、`settings.local.json`、
workspace `.claude/settings*.json`），重点看 SessionStart / PreToolUse / PostToolUse /
PermissionRequest 里 matcher 涉及 Bash / Edit / Write 的 hook：

```text
只读 / 记录型 hook                    允许
会修改 Bash tool_input                默认视为冲突，FAIL
会 deny / allow Bash、Edit、Write     WARN 或 FAIL
无法静态判断行为的 command hook       至少 WARN，显示来源文件和 command
```

用户 `~/.claude/settings.json` 里一个改写 Bash 命令的 PreToolUse hook 会和 ccnm 的 router 抢同一个
`updatedInput`，官方规则是最后完成的那个生效，结果就是命令有时在本地跑、有时在家庭机跑。

## A.7 Home runner 与 Runtime Zone

家庭机受限账户 `ccrun` 只能 read source、execute project tools、write runtime directories；不能
write source、write .git、sudo、修改 ~/.ssh。于是 `ssh ccrun@home "sed -i … src/main.rs"` 从 OS 层
直接失败，这就是真正的 single-writer enforcement。

Runtime Zone `/Users/Shared/cc-runtime/xshun` 给 ccrun 写权限，放 cargo target / pnpm cache /
coverage / tmp / logs / build output。Rust 用 `CARGO_TARGET_DIR=…/cargo-target`。

## A.8 ccnm fs

源码 topology 修改必须发生在工作机 SMB writer plane：

```bash
ccnm fs mkdir src/foo
ccnm fs move src/a.ts src/b.ts
ccnm fs remove src/old.ts
```

必须 canonicalize、workspace containment check、reject .git、reject symlink escape、audit。

## A.9 Consistency Barrier

下一次 remote Bash 不能立刻执行。工作机先算 pending 文件的 SHA256，把 `path → expected hash` 放进
runner payload；家庭机 `ccnm runner exec` 从本地 filesystem 重新 hash，全部相等才执行，否则
CCNM_E_COHERENCE 并且**不执行原 command**。

这是硬门禁，避免：

```text
Claude Edit → SMB 尚未让家庭机看到新版本 → cargo test 读旧文件 → Claude 根据旧错误继续改
```

hash mismatch 必须 fail closed，不能 `sleep 1 && retry forever`；V1 最多短暂重试几次再明确停止。

## A.10 Workspace identity 与 Epoch

源码 root 内 `.ccnm-workspace-id`（UUID）。工作机透过 SMB 读，家庭机 runner 本地读，不一致就
CCNM_E_WRONG_WORKSPACE，任何 command 都不执行。防止 mount 掉了、mount 到错误目录、SSH 去错机器、
mount point 变成普通空目录。

每次 `ccnm run` 生成 epoch UUID；`ccnm maintenance` 更新 epoch；旧 session 再调 `ccnm exec` 直接
CCNM_E_STALE_EPOCH，`git switch` 之后旧 session 不可能继续在新 workspace 状态上工作。

## A.11 Maintenance mode

```text
stop/park Claude session → mark maintenance → home: git switch / pull / pnpm install / cargo fmt /
codemod → work: SMB remount/resync → run consistency test → new epoch → READY
```

## A.12 SMB：只用 macOS 系统接口

2026-09-03 在 macOS 15 / Darwin 25.3 的 man page 上核实过：

```text
mount -t smbfs //ccuser@<home>/xshun /Users/Shared/cc-workspaces/xshun
    走系统 mount(8)，由它调 mount_smbfs。密码从 Keychain / nsmb.conf 来，ccnm 不经手。

mount_smbfs -o 选项：
    nodatacache     关闭文件数据缓存
    nomdatacache    关闭元数据缓存
    nopassprompt    不弹密码提示；没凭据就直接失败，而不是挂在那里等输入
    soft            超时后让文件系统调用失败，而不是永久挂起
    nobrowse        不在 Finder 侧栏出现

smbutil statshares -m /Users/Shared/cc-workspaces/xshun -f Json
    结构化返回这个挂载点的 share 属性。exit 0 = 是 SMB mount，exit 64 = 不是。
    不解析 mount 命令的文本输出，也不靠"目录非空"猜。

sharing -l
    家庭机上看自己导出了哪些 share point（格式 key:<tabs>value，smb: { … } 块）。
```

`mount_mode = "coherence"` = 挂载时带 `nodatacache,nomdatacache,nopassprompt,soft,nobrowse`。代价是
工作机每次 Read 都走网络；收益是 Claude 读到的永远是家庭机当前内容。

`smbutil statshares` 的 JSON 字段名（SERVER_NAME 等）没有在真实 mount 上核实过，代码里标了 best
effort。

坑：SMB 挂载失败提示 "Authentication error" 时，是工作机 Keychain 里没有这个 `smb_user@HostName`
的密码。在工作机 Finder 里 Go > Connect to Server 连一次 `smb://<HostName>` 并勾 "Remember this
password"，之后 `nopassprompt` 才能静默成功。ccnm 不经手密码。

## A.13 Hybrid 的 Phase 划分

```text
Phase 1  Transport PoC：home→work SSH、work→home SSH、SMB mount、same absolute path、workspace identity
Phase 2  Coherence：write probe / overwrite / append / create / atomic replace / hash compare，
         ccnm test coherence xshun 要求 0 mismatch，否则禁止进入 Phase 3
Phase 3  Remote runner：ccnm runner exec；rg / git diff / cargo test PASS，sed -i / git checkout /
         cargo fmt DENIED；不碰 Claude
Phase 4  Hook prototype：只支持 SessionStart、PreToolUse Bash、PostToolUse Edit|Write
Phase 5  Barrier：人工制造 stale state，command MUST NOT execute
Phase 6  ccnm run：doctor → mount → epoch → generate Claude config → tmux → official claude → attach
Phase 7  Maintenance
```

Background Bash（`run_in_background: true`）V1 直接 deny；日用稳定后再做 `ccnm process
start/logs/input/stop`。

## A.14 Hybrid 验收与 V2 决策点

验收：Read/Edit/Write native、0 coherence mismatch、cargo test / pnpm test / git diff / rg / Docker
都在 home、remote source write / stale epoch / wrong workspace / coherence mismatch 全部 denied。

V2 决策点原来只看四个数据：SMB Read/Edit latency、coherence failures、maintenance frequency、
remote process/background demand。当时的判断是"Hybrid 足够舒服就停在 Hybrid，不要为了架构漂亮去写
MCP"。这次切换不是因为这四个数据变坏了（Hybrid 还没日用过），而是第 6 节列的结构性原因：MCP 把
第 5、6、7 条一次解决，而 Hybrid 的 barrier / identity / epoch / maintenance 全是在给 SMB 双视图
打补丁。

原来设想的演进路径（Hybrid → Hybrid + MCP search → Hybrid + MCP Read → Full MCP → 删除 SMB）
现在直接从 Full MCP 开始验证；验证不过就回到这条路径的起点。
