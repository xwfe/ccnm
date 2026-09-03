架构调整确认：暂停原来的 SMB Hybrid Phase 1，`ccnm` 主方案改为 **SSH stdio Remote Coding MCP**。

Phase 0 已完成的代码不要推倒。`config / error / process runner / doctor / CLI skeleton` 全部保留。

这次是：

```text
Hybrid Remote Workspace
        ↓
降级为 fallback

SSH stdio Remote Coding MCP
        ↓
提升为 primary architecture
```

原因已经明确：

```text
1. 所有 Anthropic 请求仍然只从工作机官方 Claude Code 发出
2. 家庭机不登录 Claude、不持有 OAuth
3. 文件/搜索/Patch/Git/构建全部在家庭机同一个 filesystem namespace
4. 删除 SMB + SSH 双通路的一致性问题
5. 删除 mount/cache/barrier/single-writer 那一大套复杂度
6. SSH 只建立一条 persistent stdio transport，不是每 tool call 新建 SSH
7. search/exec/output retention 可以在数据所在地完成，具备控制 token 的条件
```

最终目标：

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
│ ccnm mcp serve              │
│                              │
│ read_file                    │
│ list_files                   │
│ search_text                  │
│ apply_patch                  │
│ exec_command                 │
│ read_output                  │
│ workspace_info               │
│                              │
│ real project filesystem      │
└──────────────────────────────┘
```

## 0. 先做 Phase 0.1

之前的 doctor：

```text
0 FAIL
N SKIP
→ NOT READY
→ exit 30
```

语义不正确。

增加：

```text
CCNM_E_NOT_READY = 3
```

规则：

```text
有 FAIL
→ 第一个 FAIL 的领域错误码

无 FAIL，有 SKIP
→ CCNM_E_NOT_READY / 3

只有 OK/WARN
→ 0
```

状态正式定成：

```text
OK
WARN
FAIL
SKIP
```

其中：

```text
WARN 不阻止 READY
SKIP 阻止 READY
FAIL 阻止 READY
```

测试：

```text
FAIL > SKIP
SKIP-only => 3
WARN-only => 0
OK-only => 0
```

单独一个 commit。

---

# 1. 先改架构文档，不删除 Hybrid

不要把之前的 Hybrid 研究删掉。

主文档改成：

```text
Primary:
SSH stdio Remote Coding MCP

Fallback:
SMB Hybrid Remote Workspace
```

把：

```text
SMB mount
coherence benchmark
single writer
write barrier
maintenance remount
```

整体移到：

```text
Appendix / Fallback Architecture: SMB Hybrid
```

保留原因和迁移条件。

新的 Primary Architecture 不再依赖：

```text
SMB
相同绝对路径
mount_smbfs
SMB cache
source write plane
hash barrier
```

---

# 2. 新的核心 invariant

固定：

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

禁止：

```text
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

V1 MCP transport 只能：

```text
stdio over SSH
```

---

# 3. config schema 立即改成 Remote Runtime 语义

Phase 0 的 schema 还没有对外发布，所以现在改成本最低。

目标：

```toml
version = 1

[hosts.work]
ssh = "work"
# claude_config_dir = "/optional/custom/path"
# ccnm_bin = "/Users/work/.local/bin/ccnm"

[hosts.home]
ssh_from_work = "ccnm-home"
# ccnm_bin = "/Users/ccrun/.local/bin/ccnm"


[workspaces.xshun]
backend = "mcp-ssh"

work_host = "work"
runtime_host = "home"

root = "/Users/bing/Projects/xshun"

claude_permission_mode = "acceptEdits"
```

语义改变：

```text
root
```

现在明确表示：

> runtime host 上项目的真实路径。

它不需要、也不应该在工作机存在。

删除 Primary MCP backend 对：

```text
share
mount_mode
runtime_root 必须和 mount 协同
```

的依赖。

如果要保留未来 Hybrid fallback：

```toml
backend = "hybrid-smb"

share = "xshun"
mount_mode = "coherence"
```

则这些字段只在：

```text
backend = hybrid-smb
```

时合法/必填。

Primary 默认：

```text
backend = mcp-ssh
```

strict unknown-field 继续保持。

验证：

```text
work_host → 必须有 ssh

runtime_host → 必须有 ssh_from_work

root → 必须绝对路径
root → 不允许 . / ..
```

MCP mode 不再要求：

```text
work root == home root
```

这是这次架构切换的重要收益。

---

# 4. SSH 身份与 ccnm binary

仍然使用：

```text
/usr/bin/ssh
~/.ssh/config
known_hosts
ssh-agent
Tailscale
ProxyJump
```

不要引入 Rust SSH client。

两个 alias：

```text
家庭机 → 工作机:
work

工作机 → 家庭机:
ccnm-home
```

开发阶段 remote binary 固定：

```text
~/.local/bin/ccnm
```

如果非交互 shell 无法找到，则允许 config 显式配置绝对：

```toml
ccnm_bin = "/Users/.../.local/bin/ccnm"
```

不要现在实现：

```text
自动 scp
自动升级
sudo install
deployment manager
```

人工保证两边安装同版本。

---

# 5. 建立版本化 internal protocol

和之前 Phase 1 的设计一样，这部分保留。

定义：

```text
CCNM_PROTOCOL_VERSION = 1
```

内部控制请求：

```text
serde_json
 ↓
base64url no-pad
 ↓
单 argv token
```

不要通过 SSH 拼任意 shell command。

例如：

```text
ccnm internal hello --payload <TOKEN>

ccnm internal work-run --payload <TOKEN>

ccnm internal mcp-serve --payload <TOKEN>
```

远端 stdout：

```text
协议输出 / MCP 输出
```

stderr：

```text
diagnostic
```

绝不能混。

---

# 6. MCP 本身不要套在 ccnm internal JSON protocol 里面

需要区分两层：

```text
CCNM control protocol
→ launcher / hello / session setup

MCP protocol
→ Claude ↔ coding runtime
```

MCP 建立后：

```text
Claude Code
 ↓
stdio MCP JSON-RPC
 ↓
ssh stdin/stdout
 ↓
ccnm mcp serve
```

不要：

```text
MCP JSON
 ↓
再包装成 base64 CCNM JSON
 ↓
再 SSH
```

MCP JSON-RPC 直接穿 SSH stdio。

---

# 7. `ccnm run xshun` 的最终启动流程

家庭机：

```bash
ccnm run xshun
```

流程：

```text
1. load config

2. local preflight
   - workspace exists
   - ccnm version
   - work SSH config valid

3. SSH → work

4. work-side ccnm
   - hello/version
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

不要复制项目源码到工作机。

---

# 8. MCP config 的目标形态

概念上：

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
        "ccnm-home",
        "/absolute/path/to/ccnm",
        "internal",
        "mcp-serve",
        "--payload",
        "<BASE64URL>"
      ]
    }
  }
}
```

payload 包含：

```json
{
  "protocol": 1,
  "workspace": "xshun",
  "root": "/Users/bing/Projects/xshun",
  "session": "...",
  "policy": "coding"
}
```

注意：

```text
Claude Code 启动一次 MCP
      ↓
ssh process 建立一次
      ↓
整个 MCP session 共用它
```

绝不能：

```text
每个 read_file
→ spawn ssh

每个 search
→ spawn ssh
```

这个必须有测试证明。

---

# 9. OpenSSH multiplexing 与 MCP transport 分开理解

MCP stdio server 自己已经持有一条长连接：

```text
work ssh process
      ↓
home ccnm mcp serve
```

因此 MCP tool call 本身不需要 ControlMaster。

ControlMaster 主要用于：

```text
home launcher → work
doctor/hello
额外 control probe
```

仍可保留：

```text
ControlMaster=auto
ControlPersist=600
```

但不要让架构依赖“每工具调用复用 ControlMaster”。

正常 MCP runtime 应该只有：

```text
one long-lived SSH process
```

---

# 10. Claude 原生工具在 MCP mode 必须关闭

Full MCP mode 不允许模型同时拥有：

```text
native Read
native Edit
native Write
native Grep
native Glob
native Bash
```

否则会误操作工作机。

启动 Claude 时使用当前版本已验证支持的：

```text
--disallowed-tools
```

禁用：

```text
Read
Edit
Write
Grep
Glob
Bash
```

不要只靠 prompt。

最终 argv 用：

```rust
Command::args()
```

不要拼 shell string。

同时使用独立 session MCP config。

在真正编码前，再用当前工作机 `claude --help` / 实测确认：

```text
--mcp-config
--strict-mcp-config
--disallowed-tools
```

的准确 argv 形式。

如果当前版本存在差异：

```text
按实测实现
```

不要猜 CLI syntax。

---

# 11. 不要直接把 coding-tools-mcp 的 20 个工具全部暴露

这是 token 和 tool-selection 的核心原则。

Phase 1A 第一版只做：

```text
1. workspace_info
2. read_file
3. list_files
4. search_text
5. apply_patch
6. exec_command
7. read_output
```

共 7 个。

Git 暂时：

```text
exec_command("git status ...")
exec_command("git diff ...")
```

完成 viability test 后再决定是否增加：

```text
git_status
git_diff
```

不要第一版加入：

```text
history
planning
task
OAuth
image
port forward
SFTP
tunnel
multi-workspace management
GPT Actions
```

---

# 12. Tool schema 要刻意压缩

## workspace_info

输入：

```text
无 / minimal
```

输出：

```text
workspace name
repo relative root
git yes/no
platform
```

不要返回几十个 env。

---

## read_file

建议：

```text
path
start_line?
end_line?
max_lines?
max_bytes?
```

所有 path：

```text
workspace-relative
```

例如：

```text
src/main.rs
```

不要让模型看到：

```text
/Users/bing/Projects/...
```

输出带稳定行号。

默认限制，例如：

```text
max_lines = 200
max_bytes = 32 KiB
```

超过：

```text
truncated=true
next_start_line
```

绝不能默认整文件无限读。

---

# 13. list_files

输入：

```text
path?
glob?
max_entries?
include_hidden?
```

默认：

```text
max_entries = 200
```

输出相对路径。

不要返回：

```text
mtime
inode
permission
owner
```

除非实际需要。

目标是帮助模型导航，不是实现 `ls -la`。

---

# 14. search_text

这是最重要的 token 优化工具之一。

输入：

```text
query
path?
glob?
regex?
case_sensitive?
context_lines?
max_results?
max_bytes?
```

家庭机本地执行：

```text
rg
```

达到：

```text
max_results
```

立即停止。

建议默认：

```text
max_results = 50
context_lines = 2
max_bytes = 32 KiB
```

只把命中结果传回模型。

原则：

> move computation to data, move answers back, not files.

---

# 15. apply_patch

不要提供：

```text
write_file(full_content)
```

作为主写入接口。

采用 coding-tools-mcp 类似的：

```text
apply_patch
```

支持至少：

```text
Add
Update
Delete
Move
dry_run
```

正式 V1 必须具备：

```text
workspace containment
stale baseline detection
same-directory temp
atomic replacement
失败不留下半写状态
```

Phase 1A 不要凭空重新设计 Patch 语义。

先研究：

```text
coding-tools-mcp
src-tauri/src/tools/
```

和其 runtime contract。

规则：

### 第一优先

只参考 contract，自主实现。

### 如果直接抽取/复制实现

必须：

```text
记录 upstream commit
记录文件 provenance
遵守 Apache-2.0
保留需要的 LICENSE / NOTICE / attribution
```

不要悄悄 copy。

---

# 16. exec_command

这是第二个 token 成本核心。

输入至少：

```text
cmd
cwd?
timeout_ms?
max_output_bytes?
preview_bytes?
```

其中：

```text
cwd
```

同样 workspace-relative。

默认：

```text
timeout = reasonable bounded value
preview <= 16 KiB
```

长输出不能完整返回模型。

例如：

```text
cargo test
```

产生 2 MB 输出：

不要：

```text
2 MB → tool result
```

而应：

```text
status
exit_code
short preview
output_ref
```

完整输出留在家庭机：

```text
~/.local/state/ccnm/runtime/<session>/
```

Claude 需要时：

```text
read_output
```

分页读取。

---

# 17. read_output

输入：

```text
output_ref
stream
offset
limit
```

默认：

```text
limit <= 32 KiB
```

必须：

```text
offset-based
stable
bounded
```

不能重复把前面的 output 每次重新发一遍。

---

# 18. `structuredContent` 也必须 bounded

不要错误假设：

> structuredContent 不算 token。

CCNM 不应该依赖 client 是否把它完整送给模型。

规则：

```text
content
→ concise

structuredContent
→ 同样 bounded

large payload
→ local retention + output_ref
```

不要：

```text
content 只有摘要
structuredContent 却放 2MB stdout
```

那只是把问题藏起来。

---

# 19. Workspace root security

MCP server startup 时：

```text
canonicalize configured root
```

之后：

```text
read/list/search/patch
```

全部使用 workspace-relative path。

拒绝：

```text
absolute path
../
symlink escape
NUL
```

`.git`：

```text
普通 file tool
→ 禁止修改
```

Git 操作只能：

```text
exec_command/git tool
```

进入。

---

# 20. `exec_command` 是真正的安全边界难点

必须明确：

```text
path validation
```

保护不了：

```bash
cat ~/.ssh/id_ed25519
curl ...
rm -rf ...
```

因为 shell command 本身可以跳出 workspace。

所以 Production V1 不允许只靠 command regex。

推荐最终 runtime 使用家庭机专用 Unix 用户：

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

如果需要：

```text
ACL/group
```

只给它目标项目和 runtime directory 权限。

Phase 1A benchmark 可以在专门 fixture repo 上先用当前用户。

但：

> 在真实项目日用之前，dedicated runtime identity 是硬门禁。

---

# 21. 家庭机网络边界也要写进设计

硬约束仍然是：

```text
api.anthropic.com
只能工作机访问
```

MCP server 自身绝不包含：

```text
Anthropic SDK
OAuth
model API
```

启动 runner 时清理：

```text
ANTHROPIC_*
CLAUDE_*
CLAUDE_CODE_OAUTH_TOKEN
CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST
```

并检查 SSH config 不转发：

```text
SendEnv ANTHROPIC_*
SendEnv CLAUDE_*
```

但必须在文档明确：

> `exec_command` 是通用 shell，因此仅靠 CCNM tool policy 不能证明任意子进程永远不会主动访问 Anthropic。

如果该网络出口约束是绝对合规边界，Production gate 应要求：

```text
ccrun 账户/执行沙箱没有公网 egress
```

或者至少由 OS/network policy 阻断 Anthropic。

不要把静态 command deny 写成“网络安全边界”。

Phase 1A fixture benchmark 不需要联网。

---

# 22. 一个 Full MCP 特有问题：项目 CLAUDE.md

切 MCP 后：

```text
Claude process cwd = 工作机
真实 repo = 家庭机
```

因此不能继续假设工作机 Claude 会自动加载家庭机：

```text
CLAUDE.md
.claude/rules/
.claude/skills/
```

这个必须成为 Phase 1A gate。

第一顺位验证：

```text
MCP initialize response.instructions
```

是否能让 Claude Code 稳定得到：

```text
CCNM remote workspace instructions
+
家庭机 root CLAUDE.md
```

先只验证 root：

```text
CLAUDE.md
```

不要一次复制整个 Claude config model。

要求：

```text
instructions bounded
```

例如最大：

```text
8–16 KiB
```

过大明确提示，不静默塞进 context。

---

# 23. 如果 MCP instructions 不足

再实现：

```text
work-side shadow workspace
```

路径：

```text
~/.local/state/ccnm/shadow/<workspace-id>/
```

只允许同步：

```text
CLAUDE.md
.claude/rules/
```

以后再研究：

```text
.claude/skills/
```

绝不复制：

```text
src/
.git/
node_modules/
Cargo target/
```

shadow workspace 每次 `ccnm run` 前重新生成。

它只是：

```text
project metadata projection
```

不是 project mirror。

---

# 24. User-level Claude 配置继续使用工作机默认目录

之前已经定了：

```text
V1 不默认设置 CLAUDE_CONFIG_DIR
```

保持不变。

因此：

```text
工作机 ~/.claude/CLAUDE.md
user settings
user skills
```

继续按普通 Claude Code 工作。

`claude_config_dir` 仍然只是 opt-in。

MCP mode 不改变这条决定。

---

# 25. Hook 在新架构里不是核心

原 Hybrid 需要：

```text
PreToolUse Bash rewrite
PostToolUse Write tracking
SessionStart
Barrier
```

Full MCP 后：

```text
Bash 已禁用
Read/Edit/Write 已禁用
```

所以这些 Hook 全部从核心 runtime 移除。

如果最后 project context 需要 SessionStart 注入：

```text
只用一个非常小的 SessionStart hook
```

不要重新建立 Hook routing architecture。

---

# 26. 不再需要的旧 Hybrid 组件

Primary MCP 路线删除/停止实现：

```text
SMB mount manager
mount_smbfs
smbutil statshares
coherence profiles
nodatacache
nomdatacache

same absolute path invariant

single writer

PostToolUse pending file set

SHA barrier

SMB health marker

mount identity

source write plane

Bash SSH rewrite
```

这些只保留在：

```text
Hybrid fallback appendix
```

不要继续在 primary code path 预留抽象。

YAGNI。

---

# 27. 新的 Phase 划分

## Phase 0

已经完成。

---

## Phase 0.1

```text
CCNM_E_NOT_READY
doctor status semantics
```

完成后 commit。

---

# Phase 1A — Architecture viability spike

这一阶段的目标不是完成产品。

只回答：

> SSH stdio Remote Coding MCP 是否值得取代 Hybrid？

先对 `lengsukq/coding-tools-mcp` 做代码研究：

```text
src-tauri/src/tools/
src-tauri/src/mcp/
SPEC / runtime contract
```

输出一份：

```text
docs/research/coding-tools-mcp.md
```

至少记录：

```text
read semantics
search result caps
patch semantics
exec output retention
git semantics
stdio 是否有现成 headless entry
Tauri coupling
可复用模块
license/provenance
```

注意：

不要因为它“看起来能用”就整个引入 dependency。

---

# 28. Phase 1A 先判断现有项目能否直接做 baseline

如果 coding-tools-mcp 当前已有：

```text
headless stdio MCP server
```

可以临时作为 benchmark baseline。

但：

```text
不集成
不 vendor
不把它变成 ccnm runtime dependency
```

只用于比较。

如果它没有方便的 headless stdio：

```text
不要为了运行它把 Tauri/Desktop 搬进 ccnm
```

直接继续做最小 ccnm MCP spike。

---

# 29. Phase 1B — Persistent SSH stdio

只实现：

```text
MCP initialize
tools/list
workspace_info
```

然后证明：

```text
Claude/work
 ↓
one SSH
 ↓
home MCP server
```

可以持续多次 JSON-RPC request。

硬测试：

```text
连续调用 workspace_info 100 次

home 上只存在一个对应 sshd session / ccnm MCP process
work 不产生 100 个 ssh process
```

记录：

```text
connect cold latency
warm MCP call p50/p95
RTT
```

此阶段还不读真实项目。

---

# 30. Phase 2 — Minimal Coding Runtime

按顺序实现：

```text
read_file
list_files
search_text
apply_patch
exec_command
read_output
```

每完成一个工具：

```text
schema test
path-policy test
size-limit test
error-semantic test
real fixture integration test
```

不要先做 Git 专用工具。

---

# 31. `apply_patch` 是 Phase 2 最后一个文件工具

不要为了快速 Demo 先写：

```text
write_file
```

然后承诺未来换 patch。

真正能修改真实项目之前：

```text
apply_patch correctness
```

必须达到门禁。

测试至少：

```text
add
update
delete
move

CRLF/LF
UTF-8
no final newline

stale baseline
partial failure
multi-file failure

symlink escape

.git reject
```

任何 partial write 都不允许。

---

# 32. Phase 3 — Claude Integration

工作机正式生成：

```text
mcp.json
settings.json
```

启动 official Claude Code。

关闭：

```text
Read
Edit
Write
Grep
Glob
Bash
```

验证 Claude 能完成：

```text
理解项目
→ search
→ read
→ patch
→ exec test
→ read output
```

这一阶段同时解决：

```text
root CLAUDE.md project context
```

问题。

---

# 33. Phase 4 — Benchmark

这是决定是否正式放弃 Hybrid 的门禁。

建立一个固定 benchmark fixture：

```text
tests/fixtures/remote-coding-project
```

在：

```text
工作机
家庭机
```

各放一份相同的小型 repo。

不要拿随时变化的真实项目做基准。

使用：

```text
同一 Claude model
同一 prompt
新 session
相同 permission settings
相同 repo revision
```

---

# 34. Micro benchmark

至少：

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

分别测：

```text
home-local stdio
SSH stdio
```

这样能分离：

```text
runtime cost
network cost
```

记录：

```text
p50
p95
max
request bytes
response bytes
```

---

# 35. Task benchmark

固定任务：

```text
1. 找到指定函数
2. 搜索调用位置
3. 阅读 2–4 个相关文件
4. 修改一个明确 bug
5. 运行测试
6. 检查变更
```

比较：

```text
A. 工作机 local fixture + native Claude tools

B. 家庭机 fixture + ccnm Remote MCP
```

至少重复：

```text
5 次
```

如果成本允许：

```text
10 次
```

记录：

```text
wall clock
tool call count
input tokens
output tokens
cache tokens（如果 CLI usage 暴露）
MCP request bytes
MCP response bytes
最大单次 tool result
失败/重试次数
```

---

# 36. Token 数据不要自己猜

先验证当前 Claude CLI：

```text
-p
--output-format json / stream-json
```

实际是否暴露：

```text
usage
input tokens
output tokens
cache fields
```

按当前 2.1.259 实际输出实现 parser。

如果某项拿不到：

```text
明确写 unavailable
```

不要用：

```text
bytes / 4
```

冒充真实 token。

可以另外记录：

```text
schema bytes
tool result bytes
```

作为工程指标，但标明它不是 token。

---

# 37. Tool schema budget

把：

```text
tools/list
```

完整 serialize 后记录：

```text
schema bytes
```

目标：

```text
7 个 core tools
总 schema 尽量 <= 16 KiB
```

如果明显超过：

```text
先缩 description/schema
```

不要为了“解释更完整”写几百字 tool description。

工具行为放文档和 runtime error semantics，不全部塞 schema。

---

# 38. Token acceptance gate

不要承诺 MCP 一定更省。

正式目标：

```text
Remote MCP total token usage
≤ native baseline + 15%
```

如果：

```text
<= native baseline
```

说明 token 优化成功。

如果：

```text
+0–15%
```

可以接受，再判断稳定性收益是否值得。

如果：

```text
> +15%
```

不能直接 promote。

先检查：

```text
tool schema 过大？
read 默认太多？
search 返回太多？
exec output 太多？
错误导致重复调用？
模型不会选工具？
```

优化后重新跑。

---

# 39. Latency acceptance gate

不要拿 RTT 本身当唯一判断。

重点：

```text
small read
search
patch
exec startup
完整开发 loop
```

目标不是“每 tool < 某个绝对数字”，而是：

```text
remote overhead ≈
一个 SSH stdio round trip + serialization
```

如果发现：

```text
一个 read_file
产生多次 SSH 往返
```

属于架构 bug。

另外记录一个 UX 门禁：

```text
普通 read/search/tool call
不应出现明显 > 250ms 的额外 transport delay
```

如果有，先定位。

---

# 40. Consistency gate

Remote MCP 最大优势必须实测。

循环：

```text
apply_patch
→ 立即 exec_command 读取/编译
```

至少：

```text
100 次
```

预期：

```text
100% 看见最新内容
```

这里不应该存在：

```text
SMB cache mismatch
```

如果有 mismatch，就是 ccnm runtime 自己的 bug。

---

# 41. Phase 5 — Production Safety

只有 benchmark 通过才进入。

此时建立：

```text
ccrun
```

专用家庭机账号。

验证：

```text
项目可读写
项目 toolchain 可运行

无 sudo
无 Claude credential
无个人 SSH private key
无浏览器 credential
```

如果项目需要 Git credential：

```text
单独设计最低权限 credential
```

不要直接共享用户整个 Keychain/SSH agent。

---

# 42. Production exec policy

区分：

```text
safe
ask
deny
```

至少默认 deny：

```text
sudo
su
ssh
scp
rsync 到任意外部 host
直接启动 claude
读取 ~/.ssh
读取系统 credential
```

但文档明确：

> command parser 不是 sandbox。

真正生产安全依赖：

```text
dedicated OS identity
filesystem ACL
network policy
```

---

# 43. Phase 6 — Terminal UX

benchmark 和安全都通过以后才实现完整：

```bash
ccnm run xshun
ccnm attach xshun
ccnm doctor xshun
ccnm status xshun
ccnm stop xshun
```

默认最终：

```text
家庭机 shell
 ↓
ccnm run
 ↓
SSH TTY → work
 ↓
tmux ccnm-xshun
 ↓
official claude
 ↓
SSH stdio MCP → home
```

网络断开：

```text
work Claude/tmux
home MCP lifecycle
```

需要明确处理。

不要在前面 spike 阶段先做漂亮 UX。

---

# 44. tmux 与 MCP lifecycle

这里后期要注意：

如果：

```text
家庭机 terminal → work tmux
```

断开，

Claude 仍然活着。

那么：

```text
work Claude → home MCP SSH
```

也应该继续活着。

这是好事。

因此 MCP transport 的生命周期绑定：

```text
Claude process
```

而不是家庭机 outer SSH TTY。

`attach` 时只重新：

```text
attach work tmux
```

不要重新创建第二个 MCP server。

---

# 45. Phase 7 — Tool Parity

只有真实日用证明需要才增加：

```text
git_status
git_diff

write_stdin
kill_session

view_image
```

优先顺序：

```text
git_status
git_diff

process interaction

image
```

不要增加：

```text
history/planning/task
```

Claude Code 已经有自己的 session/context workflow。

CCNM 不做第二套 agent harness。

---

# 46. Git 专用工具是否值得加，用 benchmark 决定

如果：

```text
exec_command("git diff")
```

经常返回很多无关输出，增加：

```text
git_status
git_diff
```

是有意义的。

因为它们可以：

```text
bounded
path-filtered
structured
```

节省 token。

如果普通 exec 已经够好：

```text
不要为了 API 完整度增加工具。
```

---

# 47. coding-tools-mcp 的使用原则

把它定义成：

```text
architecture reference
runtime contract reference
benchmark baseline
```

而不是：

```text
ccnm dependency
```

Phase 1A 完成前不要：

```text
git subtree
git submodule
copy src-tauri
添加 Tauri
添加 Node
```

CCNM production runtime 仍然要求：

```text
single Rust binary
```

家庭机不需要：

```text
Node/Bun
Tauri/WebView
```

来运行 ccnm 本身。

---

# 48. 如果后续决定复用其 Rust 工具代码

先提交一份：

```text
docs/third-party/coding-tools-mcp.md
```

记录：

```text
repository
commit
license
copied/derived modules
modifications
```

再做代码迁移。

不要先 copy 后补 provenance。

---

# 49. Rust 项目结构调整

Phase 1A 不要立刻拆十个 crate。

保留：

```text
crates/
├── ccnm-cli
└── ccnm-core
```

先在 core：

```text
src/
├── protocol/
├── ssh/
└── mcp/
```

验证架构。

等 Minimal Coding Runtime 边界稳定后再拆：

```text
ccnm-mcp
```

不要为了架构图漂亮提前拆 crate。

---

# 50. 依赖策略

现有：

```text
clap
serde
toml
tracing
tracing-subscriber
```

Phase 0.1 / 1 可增加：

```text
serde_json
base64
uuid
```

MCP server：

优先调查当前 Rust MCP SDK 是否能：

```text
stdio
initialize
tools/list
tools/call
cancellation
```

以很小依赖完成。

如果 SDK 太重或行为不透明：

```text
先报告
```

不要擅自手写半套 MCP protocol，也不要擅自引入大型 async stack。

如果 MCP SDK 合理需要：

```text
tokio
```

这次允许重新评估。

之前“V1 不用 tokio”是 Hybrid 架构下的决定，不是教条。

---

# 51. Doctor 重新定义

Primary MCP 模式最终检查：

```text
Home workspace              OK
Home ccnm                   OK

Work SSH                    OK
Work ccnm                   OK

Claude Code @ work          OK
Claude auth @ work          OK

Reverse SSH                 OK
Remote MCP handshake        OK
Workspace root              OK
Workspace policy            OK

Project instructions        OK/WARN

Native tools disabled       OK

Runtime identity            SKIP / later
Network isolation           SKIP / later
Terminal session            SKIP / later
```

不再检查：

```text
SMB mount
SMB coherence
mount identity
write barrier
```

---

# 52. `doctor` 仍然 read-only

这个原则不变。

```text
ccnm doctor
```

永远不能：

```text
安装 binary
创建用户
修改 ssh config
创建 project files
启动长期 MCP
修改 permissions
```

短暂启动：

```text
probe MCP server
```

可以，

但结束 doctor 时必须清理。

---

# 53. 新 commit 建议

按职责拆：

```text
1. core: add distinct not-ready doctor exit code

2. docs: promote SSH-stdio Remote MCP; move Hybrid to fallback

3. core: adapt config schema for mcp-ssh runtime backend

4. research: document coding-tools-mcp runtime and reuse boundary

5. core: add versioned internal control protocol

6. core: add bidirectional OpenSSH probes

7. mcp: prove persistent SSH stdio handshake

8. mcp: add bounded workspace read/list/search

9. mcp: add atomic patch runtime

10. mcp: add bounded exec and retained output

11. cli: integrate work-side Claude MCP session

12. bench: add latency/token/consistency harness
```

不要为了 commit 数量机械拆。

---

# 54. 现在实际只执行到哪里

虽然上面给了完整路线，但这一轮**不要一口气做到 Phase 4**。

现在只执行：

```text
A. Phase 0.1

B. 修改主架构文档

C. 修改 config schema

D. 完成 coding-tools-mcp research

E. 实现 internal hello + 双向 SSH probe

F. 做最小 MCP stdio spike：
   initialize
   tools/list
   workspace_info

G. 证明 one persistent SSH session
```

然后停。

暂时不要实现：

```text
read_file
search_text
apply_patch
exec_command
Claude run
benchmark
```

---

# 55. 下一次汇报内容

完成上述 A–G 后停下来给我：

```text
git log --oneline

更新后的目录结构

新 config schema

Hybrid 文档如何降级

coding-tools-mcp research 结论：
- headless stdio 是否现成
- tools runtime 与 Tauri 耦合度
- apply_patch 可复用性
- exec runtime 可复用性
- license/provenance

选用的 Rust MCP 实现/SDK，以及为什么

MCP initialize 实际 request/response

tools/list 实际 schema 和总 bytes

work → home MCP：
cold connect latency

workspace_info：
100 次 p50/p95/max

证明只有一个 persistent SSH/MCP process 的证据

home → work
work → home
实际 SSH probe

cargo fmt
cargo clippy
cargo test
```

到这个停点以后，再决定：

> 是基于 [coding-tools-mcp](https://github.com/lengsukq/coding-tools-mcp) 的工具内核继续，还是 ccnm 自己实现最小 runtime。

不要提前做决定。
