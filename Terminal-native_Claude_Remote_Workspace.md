# ccnm — Terminal-native Claude Remote Workspace

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
工作机 official Claude Code
      │
      ├──── HTTPS ────► api.anthropic.com
      │
      ├──── SMB ──────► 家庭机 source
      │
      └──── SSH ──────► 家庭机 execution
```

禁止：

```text
Claude Desktop
Desktop SSH
OAuth forwarding
OAuth proxy
家庭机 claude login
自定义 Anthropic client
ccd-cli 私有协议
```

---

# 2. 技术选择

正式版本：

```text
Rust = 运行时全部核心逻辑
```

TS 不进入生产链路。

原因：

```text
单 binary
启动快
Hook 每次调用开销小
JSON/path/hash 处理安全
家庭机不需要 Node/Bun 才能运行 ccnm
工作机也不用管理额外 runtime
```

TS 可以保留在：

```text
tests/
tools/
protocol fixtures
开发期 PoC
```

但最终：

```bash
which ccnm
```

只对应一个 Rust executable。

---

# 3. 一个 binary，三个角色

这是整个项目最重要的设计。

不要发布：

```text
ccnm-client
ccnm-server
ccnm-hook
ccnm-runner
```

而是：

```text
ccnm
```

根据 subcommand 扮演不同角色。

## 家庭机 launcher

用户直接调用：

```bash
ccnm run xshun
ccnm doctor xshun
ccnm status xshun
ccnm maintenance xshun
```

---

## 工作机 controller

内部调用：

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

---

## 家庭机 restricted runner

由工作机 SSH 调用：

```bash
ccnm runner exec
ccnm runner verify
ccnm runner health
```

最终：

```text
同一份 Rust binary
       │
       ├── home launcher
       ├── work controller
       └── home runner
```

部署、版本兼容和协议升级会简单很多。

---

# 4. CLI 设计

V1 对外只暴露少量命令。

## 初始化

```bash
ccnm init
```

创建：

```text
~/.config/ccnm/config.toml
~/.local/state/ccnm/
```

---

## 检查

```bash
ccnm doctor xshun
```

Phase 1 之后的输出（一切正常时）：

```text
ccnm doctor: xshun

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

NOT READY (0 failed, 2 not checked)
```

每行四种状态：

```text
OK / INFO / WARN   不阻塞
SKIP               这个版本还没实现，或者前置项失败没法查。同样阻塞 READY
FAIL               带 CCNM_E_* 错误码和修复提示
```

exit code 取第一个 FAIL 行的错误码；没有 FAIL 就取第一个 SKIP 的。所以 doctor 骨架阶段不可能误报 READY，`ccnm run` 的 preflight 也能直接沿用这个码。

从 "Work SSH" 往下的所有信息来自**一次** `ssh work ccnm work probe` 往返：工作机在本地查 mount、透过 mount 读 identity、反向 ssh 回家庭机跑 `ccnm runner health`、跑 `claude --version` 和 `claude auth status`，打包成一个 JSON 回来。工作机不需要 config 文件，它要的参数都在请求里。

只要有关键项失败：

```text
NOT READY (N failed, M not checked)
```

## doctor 永远 read-only

这是 invariant。doctor 不挂载、不写 identity、不新建 SSH master、不改任何文件。

改变系统状态的动作都是显式子命令：

```bash
ccnm mount xshun            # 在工作机挂 SMB
ccnm workspace init xshun   # 在源码 root 写 .ccnm-workspace-id
ccnm unmount xshun
```

原因：doctor 会被 `ccnm run` 的 preflight、cron、CI 反复调用。一旦"检查顺手改了状态"，同一条命令跑两次结果就不一样，出了问题也分不清是环境本来就坏还是 doctor 弄坏的。

具体到 SSH：doctor 探活时带 `-o ControlMaster=no`。OpenSSH 文档写明这个值只复用已有 master，socket 不存在就普通连接，不会留下一个后台 master 进程。

---

## 正常使用

```bash
ccnm run xshun
```

它完成全部 preflight，然后打开：

```text
Claude Code TUI
```

---

## 恢复

```bash
ccnm attach xshun
```

如果工作机 Claude 放在 tmux 中，可以直接恢复。

---

## 状态

```bash
ccnm status xshun
```

---

## 维护

```bash
ccnm maintenance xshun
```

用于：

```text
git switch
git pull
git checkout
pnpm install
cargo update
cargo fmt
prettier --write
codemod
```

---

## 清理

```bash
ccnm stop xshun
```

以及：

```bash
ccnm unmount xshun
```

---

# 5. 配置文件

家庭机作为配置 source of truth：

```text
~/.config/ccnm/config.toml
```

例如：

```toml
version = 1

[hosts.work]
ssh = "work"
# 可选。不设就复用工作机默认的 ~/.claude，见第 10 节。
# claude_config_dir = "/optional/custom/path"

[hosts.home_runner]
# 工作机 ~/.ssh/config 里指向家庭机的 alias。它解析出的 HostName 同时用作 SMB server 地址，
# 所以 ssh 和 SMB 永远指向同一台机器。
ssh_from_work = "ccnm-home"
# 工作机挂 SMB 时用的账号（家庭机上拥有 share 的账号，不是 ccrun）。密码在工作机 Keychain。
smb_user = "fodelf"

[workspaces.xshun]
work_host = "work"
# runner_host = "home_runner"   # 默认值，指向上面的 [hosts.home_runner]

root = "/Users/Shared/cc-workspaces/xshun"
runtime_root = "/Users/Shared/cc-runtime/xshun"

share = "xshun"

mount_mode = "coherence"   # 挂载参数含义见第 39 节

claude_permission_mode = "acceptEdits"
```

不存：

```text
Claude OAuth
SSH private key
SMB password
```

secret 继续由：

```text
macOS Keychain
OpenSSH
系统 SMB credential
```

负责。

---

# 6. 路径统一

这条必须成为 ccnm invariant。

两台机器：

```text
/Users/Shared/cc-workspaces/xshun
```

必须代表同一个项目。

家庭机：

```text
/Users/Shared/cc-workspaces/xshun
        ↓
real local filesystem
```

工作机：

```text
/Users/Shared/cc-workspaces/xshun
        ↓
SMB mount
        ↓
家庭机
```

因此 Claude 看到：

```text
/Users/Shared/cc-workspaces/xshun/src/main.rs
```

家庭机 SSH runner 看到的也是：

```text
/Users/Shared/cc-workspaces/xshun/src/main.rs
```

`ccnm` V1 **不实现路径翻译**。

发现两边 root 不同：

```text
doctor FAIL
```

而不是尝试修补。

---

# 7. Source Plane 与 Execution Plane

整个架构只保留两个数据面。

## Source Plane

```text
Claude native:

Read
Edit
Write
```

↓

```text
SMB
```

↓

```text
家庭机 source
```

---

## Execution Plane

```text
Claude native Bash
```

↓

```text
ccnm PreToolUse
```

↓

```text
ccnm exec
```

↓

```text
persistent OpenSSH
```

↓

```text
ccnm runner exec
```

↓

```text
家庭机
```

包括：

```text
rg
fd
git
cargo
rustc
pnpm
bun
node
docker
tests
build
```

---

# 8. 禁掉 native Grep / Glob

Claude 不允许直接：

```text
Grep
Glob
```

因为它们会扫描 SMB。

搜索统一：

```bash
rg xxx
fd xxx
git grep xxx
```

然后通过 Bash 路由到家庭机本地 SSD。

V1 直接用 CLI 参数禁用：

```bash
--disallowed-tools Grep Glob
```

Claude Code 2.1.259 实测有这个参数。不需要为这两个工具动态生成 `permissions.deny`。

---

# 9. ccnm 不修改项目 `.claude/settings.json`

这是一个重要原则。

不能让安装 ccnm：

```text
修改 repository
写入团队 .claude/settings.json
```

`ccnm work start` 动态生成：

```text
~/.local/state/ccnm/sessions/<id>/settings.json
```

然后：

```bash
claude \
  --settings ~/.local/state/ccnm/sessions/<id>/settings.json
```

官方支持 `--settings <file>` 对当前 session 提供高优先级配置。

---

# 10. Claude config namespace：默认复用，可选隔离

V1 默认**不设置** `CLAUDE_CONFIG_DIR`。

工作机上的 ccnm 直接复用当前已经登录的 Claude Code：

```text
不制造第二份 OAuth / token 生命周期
用户现有 ~/.claude/CLAUDE.md、skills、user settings 照常生效
hooks / settings 的隔离由 --settings <session file> 完成（第 9 节），不需要换目录
```

## 可选：自定义目录

```toml
[hosts.work]
claude_config_dir = "/some/path"
```

设置后：

```text
所有 Claude 相关 preflight 和最终启动统一带 CLAUDE_CONFIG_DIR=<该路径>
doctor 在同样的环境下执行 claude auth status
未登录只报告一行，并给出人工登录命令
```

未登录时 doctor 的输出：

```text
Claude authentication   FAIL   Claude is not authenticated in configured CLAUDE_CONFIG_DIR
                               run on work: CLAUDE_CONFIG_DIR=/some/path claude auth login
```

## 为什么自定义目录必须自己登录

自定义目录拥有**独立**的 credentials / settings / CLAUDE.md / skills 生命周期。

官方 authentication 文档明确：设置了 `CLAUDE_CONFIG_DIR` 后，`.credentials.json` 放在该目录下，macOS Keychain 条目也按该目录 key，不同目录读不同条目。官方没有跨目录共享登录的方式。

2026-09-03 在 Claude Code 2.1.259 上实测：空目录下 `claude auth status` 返回 `loggedIn: false`，默认目录返回 `true`。说白了就是：换目录等于换账号环境，登录不会跟过来。

所以 V1 推荐保持默认目录。

## ccnm 对认证的唯一动作

无论哪种情况，ccnm 绝不：

```text
执行或自动触发 claude auth login
复制 credentials
```

它只检查：

```bash
claude auth status
```

登录时 exit 0，未登录时 exit 1，默认输出 JSON。

---

# 11. Project CLAUDE.md 继续加载，doctor 扫描全部 settings sources

我们仍希望：

```text
CLAUDE.md
.claude/rules/
project skills
```

正常工作。

因此默认：

```bash
--setting-sources user,project,local
```

Claude 官方说明 `project` source 负责加载项目级 `CLAUDE.md`、rules、skills、hooks 和 settings。

## doctor 必须扫描哪些文件

V1 默认使用默认 Claude config（第 10 节），启动时 `user` / `project` / `local` 三个 source 都会参与 session。所以 `ccnm doctor` 不能只看 repo 内的 `.claude/settings*.json`，必须扫描实际会参与当前 session 的全部 settings，至少：

```text
~/.claude/settings.json
~/.claude/settings.local.json     # 如果当前版本/来源实际适用

<workspace>/.claude/settings.json
<workspace>/.claude/settings.local.json
```

如果设置了 `claude_config_dir`，user settings root 相应切到该目录。

## 重点看哪些 hook

事件：

```text
SessionStart
PreToolUse
PostToolUse
PermissionRequest
```

特别是 matcher 涉及：

```text
Bash
Edit
Write
```

## 分类

```text
只读 / 记录型 hook                    允许，doctor 显示 INFO
会修改 Bash tool_input                默认视为冲突，FAIL
会 deny / allow Bash、Edit、Write     WARN 或 FAIL，按是否破坏 ccnm invariant 判断
无法静态判断行为的 command hook       至少 WARN，并显示来源文件和 command
```

不这么做的后果：用户 `~/.claude/settings.json` 里一个改写 Bash 命令的 PreToolUse hook 会和 ccnm 的 router 抢同一个 `updatedInput`，官方规则是最后完成的那个生效，结果就是命令有时在本地跑、有时在家庭机跑，而 Claude 看不出区别。

**不要尝试自动改写或合并用户已有 hooks。** 发现冲突就报出来，让人处理。

---

# 12. SessionStart Hook

每次 session 开始：

```text
ccnm hook session-start
```

通过 `additionalContext` 告诉 Claude，只有这几行：

```text
CCNM remote workspace active.
Source edits use the mounted workspace.
Bash executes on the home runner.
Do not mutate source from Bash.
Source-mutating git/formatter/install commands require CCNM maintenance mode.
```

Claude Hooks 官方支持 SessionStart 返回 `hookSpecificOutput.additionalContext`。

保持很短的原因：hook 输出字符串上限 10,000 字符，超过会被换成一个文件路径加预览，Claude 看到的就不是你写的那段话。而且完整规则由 ccnm policy 和 OS 权限保证，不靠 prompt。

---

# 13. PreToolUse 是核心 router

收到：

```json
{
  "tool_name": "Bash",
  "cwd": "/Users/Shared/cc-workspaces/xshun",
  "tool_input": {
    "command": "cargo test"
  }
}
```

`ccnm` 分类：

```text
LOCAL_SAFE
REMOTE
DENY
CCNM_INTERNAL
```

---

# 14. LOCAL_SAFE

极少。

第一版只考虑：

```text
纯 cd
```

例如：

```bash
cd packages/core
```

因为它需要修改 Claude 工作机侧 cwd。

不能远程执行，否则：

```text
home cwd changed
work cwd unchanged
```

发生 namespace 分叉。

---

# 15. REMOTE

绝大多数 Bash：

```text
cargo test
pnpm test
rg xxx
git status
git diff
node ...
bun ...
docker ...
```

改写为：

```text
ccnm exec <opaque-session-payload>
```

不是：

```bash
ssh home '原始命令'
```

Claude Code 官方 `PreToolUse.updatedInput` 可以替换完整 tool input，因此这一层不需要改 Claude 本身。

---

# 16. 为什么 payload 不能直接 shell quote

内部 descriptor：

```json
{
  "protocol": 1,
  "session": "...",
  "epoch": "...",
  "workspace": "xshun",
  "cwd": "/Users/Shared/cc-workspaces/xshun",
  "command": "cargo test",
  "timeout_ms": 120000
}
```

序列化：

```text
JSON
 ↓
base64url
 ↓
ccnm exec --payload ...
```

避免：

```text
quotes
heredoc
$
|
&&
newline
```

二次 shell escaping。

---

# 17. ccnm exec

工作机：

```bash
ccnm exec --payload XXX
```

负责：

```text
验证 session
验证 epoch
读取 pending writes
建立 consistency barrier
调用 SSH
```

然后：

```bash
/usr/bin/ssh -T ccnm-home \
    ccnm runner exec --payload XXX
```

底层继续使用：

```text
OpenSSH
```

而不是 Rust SSH library。

---

# 18. SSH：ccnm 拥有 multiplexing，不拥有 identity / config

这是 invariant。分工：

```text
用户 ~/.ssh/config    决定 Host、HostName、User、IdentityFile、ProxyJump、Tailscale 地址
ccnm                  只在命令行追加 ControlMaster / ControlPath / BatchMode / 安全覆盖项
```

用户自己维护，ccnm 只读：

```sshconfig
Host ccnm-home
    HostName <tailscale-name>
    User ccrun
    IdentityFile ~/.ssh/ccnm_ed25519
```

ccnm 每次调用 ssh 时追加（用 `Command::args()`，不写进任何 config 文件）：

```text
-o BatchMode=yes
-o ControlMaster=auto
-o ControlPath=~/.local/state/ccnm/ssh/%C
-o ControlPersist=10m
-o ServerAliveInterval=15
-o ServerAliveCountMax=3
```

外加第 32 节的 SendEnv 覆盖。

OpenSSH 规定命令行选项优先于 `~/.ssh/config`（每个参数取第一个出现的值），所以这些追加项一定生效，用户 config 里写了别的 ControlMaster 也不会打架。

效果：Claude 每个 Bash 都复用同一条连接，不重做 handshake。

## 只用 OpenSSH 自带的能力

```text
ssh -G ccnm-home            打印最终解析出的配置，不建连接。doctor 用它显示实际会用的 HostName / User / IdentityFile
ssh -O check ccnm-home      问 master 是否活着
ssh -O exit ccnm-home       让 master 退出，ccnm stop 用
```

ccnm 不自己维护长连接协议，也不用 Rust SSH 库。OpenSSH 的 ControlMaster / ControlPersist 本身就是官方实现的连接复用，比 ccnm 自己管稳得多。

## 一个会撞的坑

macOS 上 unix socket 路径最长 104 字节（`sys/un.h` 里 `sun_path[104]`）。ControlPath 超过就报 `ControlPath too long`，看着像 ssh 坏了，实际是路径长。`%C` 展开后是 40 个十六进制字符，所以 `~/.local/state/ccnm/ssh/` 这个前缀在 HOME 正常长度时够用。doctor 应该算一下展开后的长度，超了直接 FAIL 并说明。

---

# 19. Home runner

家庭机受限账户：

```text
ccrun
```

它只能：

```text
read source
execute project tools
write runtime directories
```

不能：

```text
write source
write .git
sudo
修改 ~/.ssh
```

于是：

```bash
ssh ccrun@home \
  "sed -i ... src/main.rs"
```

从 OS 层直接失败。

这就是真正的 single-writer enforcement。

---

# 20. Runtime Zone

建立：

```text
/Users/Shared/cc-runtime/xshun
```

给 `ccrun` 写权限。

例如：

```text
cargo target
pnpm cache
coverage
tmp
logs
build output
```

Rust：

```bash
CARGO_TARGET_DIR=/Users/Shared/cc-runtime/xshun/cargo-target
```

其它 build system 尽量同样迁出 source tree。

---

# 21. Source mutation command 默认失败

家庭机 runner 没写权限，因此：

```text
cargo fmt
prettier --write
eslint --fix

git checkout
git switch
git reset
git restore
git pull

codemod
```

即使路由过去也无法直接修改 source。

`ccnm` 在 PreToolUse 还应提前识别常见命令并返回更友好的：

```text
DENY
```

而不是等 Permission denied。

---

# 22. ccnm fs

源码 topology 修改必须发生在工作机 SMB writer plane。

提供：

```bash
ccnm fs mkdir src/foo
ccnm fs move src/a.ts src/b.ts
ccnm fs remove src/old.ts
```

这个命令在工作机执行。

PreToolUse 检测：

```text
command starts with:
ccnm fs
```

标记：

```text
CCNM_INTERNAL
```

不 SSH 回家庭机。

它必须：

```text
canonicalize
workspace containment check
reject .git
reject symlink escape
audit
```

---

# 23. PostToolUse Write/Edit tracking

Claude 原生：

```text
Edit
Write
```

成功之后：

```text
ccnm hook post-tool
```

读取：

```text
session_id
file_path
```

写入：

```text
pending source set
```

例如：

```json
{
  "files": [
    "/Users/Shared/cc-workspaces/xshun/src/main.rs",
    "/Users/Shared/cc-workspaces/xshun/src/lib.rs"
  ]
}
```

官方 PostToolUse 会提供 `tool_input.file_path`。

---

# 24. Consistency Barrier

下一次 remote Bash：

```text
cargo test
```

不能立刻执行。

工作机先计算：

```text
SHA256(main.rs)
SHA256(lib.rs)
```

把：

```text
path → expected hash
```

放进 runner payload。

家庭机：

```text
ccnm runner exec
```

先：

```text
直接从本地 filesystem 重新 hash
```

只有全部：

```text
expected == actual
```

才执行：

```text
cargo test
```

否则：

```text
CCNM_E_COHERENCE
```

并且**不执行原 command**。

---

# 25. 为什么 barrier 是硬门禁

避免：

```text
Claude Edit
     ↓
SMB 尚未让家庭机看到新版本
     ↓
cargo test 读取旧文件
     ↓
Claude 根据旧错误继续修改
```

一旦出现：

```text
hash mismatch
```

必须 fail closed。

不能自动：

```text
sleep 1 && retry forever
```

V1 最多短暂重试几次，再明确停止。

---

# 26. Workspace identity

源码 root 内：

```text
.ccnm-workspace-id
```

例如：

```text
550e8400-e29b-41d4-a716-446655440000
```

工作机通过 SMB 读取。

家庭机 runner 本地读取。

两边不一致：

```text
CCNM_E_WRONG_WORKSPACE
```

任何 command 都不执行。

防止：

```text
mount 掉了
mount 到错误目录
SSH 去错机器
本地 mount point 变成普通空目录
```

---

# 27. Epoch

每次：

```bash
ccnm run xshun
```

生成：

```text
epoch UUID
```

如果执行：

```bash
ccnm maintenance xshun
```

epoch 更新。

旧 Claude session 再调用：

```text
ccnm exec
```

直接：

```text
CCNM_E_STALE_EPOCH
```

这样 `git switch` 之后旧 session 不可能继续在新 workspace 状态上工作。

---

# 28. Maintenance mode

运行：

```bash
ccnm maintenance xshun
```

流程：

```text
stop/park Claude session

mark maintenance

home:
    git switch / pull
    pnpm install
    cargo fmt
    codemod
    ...

work:
    SMB remount/resync

run consistency test

new epoch

READY
```

退出：

```bash
ccnm maintenance --finish xshun
```

---

# 29. tmux session persistence

纯终端环境非常建议直接支持。

默认：

```bash
ccnm run xshun
```

工作机实际上：

```text
tmux session:
ccnm-xshun
       │
       └── claude
```

家庭机 SSH attach。

网络断开：

```text
Claude session 不死
```

恢复：

```bash
ccnm attach xshun
```

不依赖 Desktop。

---

# 30. Claude 启动命令

最终由 `ccnm` 生成，用户不需要自己写：

```bash
claude \
  --settings "$CCNM_SESSION/settings.json" \
  --setting-sources user,project,local \
  --permission-mode acceptEdits \
  --disallowed-tools Grep Glob
```

配置了 `claude_config_dir` 时，额外带环境变量 `CLAUDE_CONFIG_DIR=<该路径>`；没配置就不带（第 10 节）。

具体 argv 以当前 CLI 实测格式为准。代码里不拼 shell 字符串，用 `Command::args()` 逐个传参，环境变量用 `Command::env()`。

并由：

```text
tmux
```

承载。

---

# 31. Background Bash V1 明确不支持

Hook 看到：

```json
"run_in_background": true
```

直接：

```text
deny
```

原因：

```text
Claude background lifecycle
+
work wrapper
+
SSH
+
home process
```

V1 没必要同时解决。

等日用 Hybrid 稳定后再实现：

```text
ccnm process start
ccnm process logs
ccnm process input
ccnm process stop
```

---

# 32. 网络请求边界

`ccnm runner` 启动 command 前主动删除：

```text
ANTHROPIC_*
CLAUDE_CODE_OAUTH_TOKEN
CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST
```

SSH 这一侧，ccnm 每次调用都追加：

```text
-o SendEnv=-ANTHROPIC_*
-o SendEnv=-CLAUDE_*
```

OpenSSH 支持用 `-` 前缀清掉用户 `~/.ssh/config` 里已经写了的 SendEnv 模式，所以即使用户全局配了 `SendEnv *`，这两类变量也不会跟着 ssh 出去。家庭机 sshd 的 AcceptEnv 默认为空，是第三道保险。

家庭机不需要知道任何 Anthropic credential。

---

# 33. Repository 结构

建议第一天就按长期结构建。

```text
ccnm/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
│
├── crates/
│   ├── ccnm-cli/
│   │   └── src/
│   │       └── main.rs
│   │
│   ├── ccnm-core/
│   │   └── src/
│   │       ├── config.rs
│   │       ├── workspace.rs
│   │       ├── session.rs
│   │       ├── epoch.rs
│   │       ├── error.rs
│   │       └── paths.rs
│   │
│   ├── ccnm-hooks/
│   │   └── src/
│   │       ├── pre_tool.rs
│   │       ├── post_tool.rs
│   │       └── session_start.rs
│   │
│   ├── ccnm-transport/
│   │   └── src/
│   │       ├── ssh.rs
│   │       ├── smb.rs
│   │       └── mount.rs
│   │
│   ├── ccnm-runner/
│   │   └── src/
│   │       ├── exec.rs
│   │       ├── verify.rs
│   │       └── policy.rs
│   │
│   └── ccnm-protocol/
│       └── src/
│           ├── exec.rs
│           ├── hook.rs
│           └── version.rs
│
├── tests/
│   ├── fixtures/
│   ├── coherence/
│   ├── hook/
│   └── e2e/
│
└── tools/
    └── ts/
```

不是一开始就做很多 package。

可以先只有：

```text
ccnm-cli
ccnm-core
```

等边界稳定后再拆 crate。

---

# 34. Rust dependencies

V1 控制住依赖：

```toml
clap
serde
serde_json
toml
thiserror
uuid
sha2
base64
dirs
tracing
tracing-subscriber
```

可能再：

```text
nix
```

处理 Unix signal / process。

V1 不要：

```text
tokio
russh
ratatui
axum
```

除非真的需要。

整个程序主要是：

```text
spawn process
read JSON
validate
hash
write state
```

同步 Rust 足够。

---

# 35. TS 的定位

如果你想利用自己熟悉的 TS，可以把这些放 TS：

```text
integration-test harness
fixture generator
benchmark reporter
hook protocol fuzz generator
```

例如：

```text
tools/ts/e2e.ts
tools/ts/generate-hook-fixtures.ts
```

但：

```text
ccnm run
```

永远不要求：

```text
node
bun
pnpm
```

存在。

---

# 36. Error code 必须从第一天稳定

定义，名字和进程 exit code 一起固定：

```text
名字                       exit   含义
CCNM_E_INTERNAL             1     bug 或意外的 OS 错误，不是给用户分类用的
CCNM_E_CONFIG              10     config.toml 缺失、解析失败或校验不过
CCNM_E_VERSION             11     两台机器 ccnm 版本不一致，或 Claude Code 太旧
CCNM_E_AUTH                12     工作机 Claude 未登录
CCNM_E_WORK_UNREACHABLE    20     家庭机 SSH 不到工作机
CCNM_E_HOME_UNREACHABLE    21     工作机 SSH 不到家庭机 runner
CCNM_E_MOUNT               22     SMB share / mount 缺失或不可用
CCNM_E_WRONG_WORKSPACE     30     两边 .ccnm-workspace-id 不一致
CCNM_E_COHERENCE           31     hash 不一致，命令没有执行
CCNM_E_STALE_EPOCH         32     session epoch 过期
CCNM_E_POLICY              33     runner 不允许这条命令
```

exit 0 是成功，2 留给 clap 的用法错误。`ccnm doctor` NOT READY 时 exit code 取第一个失败检查项对应的错误码，所以 `ccnm run` 里的 preflight 失败也能直接带出原因。

加新码可以，改名或改号不行：另一台机器上可能还跑着旧版 ccnm。

Claude 收到：

```text
CCNM_E_COHERENCE:
Remote workspace does not match the mounted source view.
Command was NOT executed.
```

比：

```text
command failed
```

有用得多。

---

# 37. Audit

工作机：

```text
~/.local/state/ccnm/audit.jsonl
```

记录：

```json
{
  "session": "...",
  "epoch": "...",
  "workspace": "xshun",
  "cwd": "...",
  "original_command": "cargo test",
  "route": "home",
  "barrier_files": 2,
  "exit_code": 0,
  "duration_ms": 4253
}
```

不记录：

```text
OAuth
SSH private key
password
secret env values
```

---

# 38. Phase 0 — skeleton

目标：

```bash
ccnm --version
ccnm doctor
```

完成：

```text
Cargo workspace
config parser
error model
logging
process abstraction
```

门禁：

```text
cargo test
cargo clippy -- -D warnings
cargo fmt --check
```

---

# 39. Phase 1 — Transport PoC

只验证：

```text
home → SSH → work
work → SSH → home
SMB mount
same absolute path
workspace identity
```

命令：

```bash
ccnm doctor xshun
```

必须可靠。

这一阶段：

```text
不启动 Claude
不写 Hook
```

## 只围绕系统接口做，不自己推断状态

SSH 用第 18 节列出的 `ssh -G` / `-O check` / `-O exit`。

SMB 用 macOS 自带的这几个，2026-09-03 在 macOS 15 / Darwin 25.3 的 man page 上核实过：

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
    结构化返回这个挂载点的 share 属性。doctor 判断"挂了没有、挂的是哪个 server 的哪个 share"用它，
    不去解析 mount 命令的文本输出，也不靠"目录非空"猜。
```

`mount_mode = "coherence"` 的含义就是：挂载时带 `nodatacache,nomdatacache,nopassprompt,soft,nobrowse`。代价是工作机每次 Read 都走网络，Read/Edit 会慢；收益是 Claude 读到的永远是家庭机当前内容，Phase 2 的 coherence 测试才有意义。第 48 节的 V2 决策点跟踪的 "SMB Read/Edit latency" 就是这个代价。

不这么做的后果：自己拼 nsmb 参数或猜挂载状态，macOS 升级一次就可能错一次，而且错了 doctor 还显示 OK。

## 第一次把一个 workspace 跑起来的顺序

```bash
# 家庭机
ccnm workspace init xshun     # 在 root 里写 .ccnm-workspace-id
ccnm mount xshun              # ssh 到工作机，让它 mount -t smbfs
ccnm doctor xshun             # 只读，看表
```

`ccnm mount` 在工作机上做的事：`ssh -G ccnm-home` 拿 HostName 拼出 `//smb_user@<HostName>/xshun`，建挂载点（`/Users/Shared` 本来就可写，不需要 sudo），`mount -t smbfs -o <coherence 选项> url 挂载点`，再用 `smbutil statshares` 确认。已经挂着就直接返回，挂载点非空且不是 SMB mount 就拒绝。

## 一个会撞的坑：非交互 ssh 的 PATH

`ssh work ccnm work probe` 是通过工作机登录 shell 的 `-c` 跑的，zsh 这时只读 `~/.zshenv`，不读 `~/.zshrc`。ccnm 装在 `~/.cargo/bin` 的话远端会报 `command not found`，ssh 退出码 127。

doctor 看到 127 会直接说：

```text
Work SSH   FAIL   CCNM_E_VERSION: ccnm is not on PATH for non-interactive ssh to work
                  sshd runs the command through the login shell without ~/.zshrc; install ccnm in /usr/local/bin or export PATH from ~/.zshenv
```

`claude` 同理，所以 probe 找 claude 时除了 PATH 还会试 `~/.local/bin`、`~/.claude/local`、`/usr/local/bin`、`/opt/homebrew/bin`。

另一个：SMB 挂载失败提示 "Authentication error" 时，是工作机 Keychain 里没有这个 `smb_user@HostName` 的密码。在工作机 Finder 里 Go > Connect to Server 连一次 `smb://<HostName>` 并勾 "Remember this password"，之后 `nopassprompt` 才能静默成功。ccnm 不经手密码。

---

# 40. Phase 2 — Coherence

实现：

```text
write probe
overwrite
append
create
atomic replace
hash compare
```

命令：

```bash
ccnm test coherence xshun
```

要求：

```text
0 mismatch
```

否则：

```text
禁止进入 Phase 3
```

---

# 41. Phase 3 — Remote runner

实现：

```bash
ccnm runner exec
```

验证：

```text
rg                       PASS
git diff                 PASS
cargo test               PASS

sed -i src/...           DENIED
git checkout             DENIED
cargo fmt                DENIED
```

此时仍然不碰 Claude。

---

# 42. Phase 4 — Hook prototype

生成最小 Claude settings。

只支持：

```text
SessionStart
PreToolUse Bash
PostToolUse Edit|Write
```

官方 Hooks 通过 stdin/stdout JSON 工作，因此 Rust binary 可以直接作为 command hook，不需要中间 shell。

验证：

```text
Claude Bash("rg foo")
            ↓
ccnm
            ↓
home rg foo
```

---

# 43. Phase 5 — Barrier

实现：

```text
Edit
 ↓
pending set

Bash
 ↓
hash barrier
 ↓
runner
```

人工制造 stale state。

预期：

```text
command MUST NOT execute
```

这是正式进入日用前最重要的 integration test。

---

# 44. Phase 6 — `ccnm run`

最终打通：

```bash
ccnm run xshun
```

内部：

```text
doctor
 ↓
mount
 ↓
epoch
 ↓
generate Claude config
 ↓
tmux
 ↓
official claude
 ↓
attach terminal
```

到这里 V1 才算完成。

---

# 45. Phase 7 — Maintenance

加入：

```bash
ccnm maintenance xshun
ccnm maintenance --finish xshun
```

解决：

```text
git switch
install
formatter
codemod
```

---

# 46. V1 明确不做

非常重要。

不要 scope creep：

```text
❌ GUI

❌ Desktop integration

❌ Anthropic API client

❌ OAuth handling

❌ MCP filesystem

❌ Rust SSH implementation

❌ multi-host orchestration

❌ port forwarding manager

❌ SFTP abstraction

❌ PTY remote process manager

❌ Windows

❌ Linux first-class support
```

先针对：

```text
macOS home
+
macOS work
+
Tailscale
+
OpenSSH
+
SMB
+
official Claude Code
```

把一个场景做透。

---

# 47. V1 验收标准

必须同时满足：

### Auth

```text
家庭机无 Claude login
家庭机无 Claude OAuth
Anthropic 请求从工作机发出
```

### Filesystem

```text
Read native
Edit native
Write native
0 coherence mismatch
```

### Execution

```text
cargo test home
pnpm test home
git diff home
rg home
Docker home
```

### Protection

```text
remote source write denied
stale epoch denied
wrong workspace denied
coherence mismatch denied
```

### UX

最终日常命令不超过：

```bash
ccnm run xshun
ccnm attach xshun
ccnm maintenance xshun
ccnm doctor xshun
```

---

# 48. V2 决策点

V1 日用一段时间以后只看四个数据：

```text
SMB Read/Edit latency
coherence failures
maintenance frequency
remote process/background demand
```

如果 Hybrid 足够舒服：

```text
ccnm 就停在 Hybrid
```

不要为了架构漂亮去写 MCP。

如果开始频繁出现：

```text
SMB latency
split-brain
formatter/codemod
branch switching
PTY/background
```

才进入：

```text
ccnm workspace MCP
```

---

# 49. 将来 MCP 也不需要推翻 ccnm

到时：

```text
ccnm runner
```

已经拥有：

```text
workspace identity
path policy
execution
audit
SSH transport
epoch
session
```

只需要在它上面增加：

```text
MCP stdio adapter
```

Claude Code 官方支持本地 stdio MCP，因此可以逐步增加：

```text
fs.read
fs.grep
fs.glob
fs.patch
exec
```

而不是重新写整套项目。

演进：

```text
ccnm Hybrid
    ↓
Hybrid + MCP search
    ↓
Hybrid + MCP Read
    ↓
Full MCP workspace
    ↓
删除 SMB
```

---

# 50. 项目最终定位

`ccnm` 不应该定义成：

> Claude SSH wrapper

而应该定义成：

> **A terminal-native remote workspace runtime for Claude Code.**

它不碰模型认证。

它只负责：

```text
workspace
execution
coherence
routing
policy
session
transport
```

因此即使以后 Anthropic 官方推出真正的：

```bash
claude --ssh home
```

`ccnm` 的：

```text
doctor
policy
workspace identity
execution isolation
audit
runtime management
```

仍然有价值。