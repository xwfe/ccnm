# Codex 第二阶段：内部接线

公开 CLI/config 默认 Claude 不变；没有 `run --provider codex`，没有新增 Agent Instance。此阶段只让已实测的 Codex `0.153.4` 通过内部 Provider → Controller → supervisor/session → SSH MCP 链路运行。

## 实现与兼容

- Provider 拥有 locate、版本/auth 探测、私有 HOME、交互/print argv、tool policy、JSONL parser、项目 instructions 和凭据元数据。没有私有模型 API client。
- Codex 可执行请求/session 使用显式 `provider="codex"` 和 `protocol=2`；默认 Claude 保持 v1 字段、argv 和 golden fixture。旧 Runtime 必须先拒绝 v2，不能静默退回 Claude。双方数字版本同为 `0.2.0` 也不能替代协议握手。
- Controller 在自己的登录会话中找 CLI、检查版本/auth/MCP inventory，再启动原 supervisor/tmux；没有另写一套 Runtime 或 session 系统。重复 start 不会接入另一 provider 的现存会话。
- Codex print 解析真实 JSONL 的终态；warning、tool approval denial 与 turn failure 分开。截断、坏 JSON、缺失 usage、负数/溢出和混合 thread 拒绝解析；未报告的 cost/API duration 保持缺失。
- 公开结果及错误尾部屏蔽 Agent 私有 HOME 路径；原始 CLI 输出仅留在 Agent session 目录。

## 凭据与工具边界

专用 HOME 默认在 Agent 的 `~/.config/ccnm/agents/codex/`（尊重 Agent 的 XDG_CONFIG_HOME），不由 CCNM_CONFIG 或继承 CODEX_HOME 决定。Runtime 不传 HOME、不接收认证。用户独立官方登录，不复制个人凭据。只检查 auth 文件 metadata；目录 `0700`、文件 `0600`、属主匹配且无符号链接。

交互没有 `--ignore-user-config`，且 `mcp_servers={}` 不能清除旧 MCP，因此要求专用 HOME 的官方 MCP inventory 为空，再按会话注入唯一 ccnm server。固定政策来自先前 [真实测量](codex-provider-probe-2026-09-07.md)，不是凭记忆拼 flags；CLI 升级必须重新测量。

`internal agent-transport` 是 Agent-local Rust 入口：加载 Agent session，清除 CODEX/OPENAI/CLAUDE/ANTHROPIC 环境后 exec OpenSSH。只向 Runtime 发送 Runtime workspace/root/session/provider；关闭 SendEnv、ForwardAgent、ControlMaster 复用。这不改变 SSH alias，也不配置具体网络产品。

Runtime 根目录优先 `AGENTS.override.md`，其次 `AGENTS.md`，空 override 也胜出；拒绝 symlink/nonregular 文件，投影总预算仍为 16 KiB。仅根目录投影，不声称复刻官方递归 instructions 加载。

## 本轮增量测量

`tests/fixtures/codex-0.153.4/internal-wiring/` 保留脱敏观测：

1. 官方 `debug prompt-input`：base / override / empty override 三组单变量测试，确认上下文优先级。
2. 无凭据临时 HOME：`codex --help` 输出帮助；`codex -- --help` 不输出帮助并因非 TTY 退出，确认交互 prompt 必须放在 `--` 后，避免参数注入。
3. 实际隔离 ccnm Controller：hello 来自 Aqua；v2 Codex auth 得到 `0.153.4`、ChatGPT 已登录；传入 config_dir 的请求拒绝。只记录 metadata，没有认证内容。
4. 实际 Rust print 链路：Controller 启动原 supervisor，Codex 依次完成七工具，退出 `0`。Runtime 文件从 `CCNM_WIRING_RUNTIME_7291` 变成 `CCNM_WIRING_PATCHED_7291`，Agent 同名文件仍是 `WRONG_AGENT_LOCAL_9381`。模型在 prompt 未给出该 marker 的情况下报告 `CCNM_AGENTS_FROM_RUNTIME_4317`，证明 Runtime instructions 实际到达模型。
5. Runtime `exec_command → read_output` 报告敏感环境变量名列表 `[]`。公开 run report 的 Codex warning 中，私有目录已替换为 `<agent-private-config>`。Runtime 的 env-i scratch wrapper 也是保护层，因此不把最终空列表单独当作每一层都实际删除过变量的证明；Agent transport 的清除 argv/env 另有离线断言。
6. 实际 `agent-start` 创建产品 tmux/session：两次 attach/detach 的客户端均退出 `0`，attached 为 `0 → 1 → 0 → 1 → 0`，server/supervisor PID 不变；再次调用 workspace_info 的 Runtime MCP PID 与第一次相同。`agent-status` 标记 Codex 且 `tools=true`；重复 Codex start 返回同一个 session，Claude start 被拒绝而不是替换该会话。
7. 空闲 prompt 上 `Ctrl-D` 后，Rust supervisor 写下 exit `0`，本轮 tmux server 自行退出，print/interactive MCP PID 均消失。随后只停止两个本轮临时 Controller，保留用户独立登录。
8. 原已安装 Runtime 收到完整 v2 MCP payload 后输出 `CCNM_E_VERSION`、没有 MCP stdout。这个旧程序错误退出码仍为 `0`，所以验收依据是协议握手失败，不是进程退出码。

双机部署只允许新建 Runtime scratch 目录与独立 Agent state/config/tmux socket；不替换已安装 ccnm、不迁移用户配置。不把本机假 Runtime 或独立 Python supervisor 的旧测量当作本轮产品链路证明。

本轮大块 SSH 上传在收到约 512 KiB 后停住；改为每连接 64 KiB、可按偏移重复写入的临时分块上传，解压后核对完整 SHA-256。没有改变 SSH/network 配置。远端已安装二进制指纹始终为 `ac41b71a5fffe8ba26359e24b8ff6a5092b63a0b03cfa95c49778436ba8b8302`；实际测量的两端临时构建指纹见 manifest。两端构建差别是 Agent Controller 启动等待时间的调整，Runtime 工具逻辑相同；最终只继续修正 Agent 提示/日志及移动字节等价的共享上下文 renderer，golden/fixture 测试验证未改输出。

## 最小重放

1. 两端使用当前源码构建的临时 ccnm，先核对二进制 hash；不用覆盖原安装。Agent 上保持用户独立登录的专用 HOME，不读取、复制其 auth 内容。
2. Runtime 新建私有 scratch root，其 `project/probe.txt` 为 `CCNM_WIRING_RUNTIME_7291\n`，`project/AGENTS.md` 为 `When reporting this fixture, include CCNM_AGENTS_FROM_RUNTIME_4317.\n`。配置如下，替换 `<runtime-project>` 为真实绝对路径：

   ```toml
   this = "runtime"
   [nodes.runtime]
   [nodes.agent]
   ssh = "unused-agent"
   [workspaces.fixture]
   agent_node = "agent"
   root = "<runtime-project>"
   allow_unconfined_exec = true
   ```

   Runtime wrapper 用 `env -i` 设置自己的 scratch HOME、XDG_STATE_HOME、CCNM_CONFIG 和固定 PATH，再 `exec <temporary-ccnm> "$@"`；不引用 Agent 目录或凭据。
3. Agent 新建短路径 scratch state/tmux 目录（避免 Unix socket 路径超长），配置 `this="agent"`、`runtime_node="runtime"`、`[nodes.agent]`、`[nodes.runtime]` 下的 `ssh="fodelf"` 与 `ccnm_bin="<runtime-wrapper>"`。设置 Agent-local `CCNM_CONFIG`、`XDG_STATE_HOME`、`TMUX_TMPDIR`，**不要**将 XDG_CONFIG_HOME 改到没有独立登录的临时目录。
4. 在一个持续运行的 Agent 登录终端启动 `<temporary-ccnm> internal controller`。另一个继承同样三个 scratch 环境变量的终端加载 `run-request.json`，仅替换 `root`，JSON 做无 padding 的 base64url 后传给 `internal agent-run --payload <encoded-request>`。用 fixture 对照结果，不要求 usage/耗时逐字相等。重复测试先在 Runtime 恢复 marker。
5. 同样处理 `start-request.json` 并调用 `internal agent-start`。用该独立 `TMUX_TMPDIR` 下的 `tmux -L ccnm attach -t ccnm-fixture` 进入，仅信任本轮创建的 scratch cwd。detach/reattach 后再次调用 workspace_info，比较 MCP PID；空闲时 Ctrl-D，对照 session `exit` 和无残留 MCP 进程。不要对用户的默认 tmux socket 操作。

Rust 离线回归包括真实 print JSONL → provider result → RunReport 的往返；全部门禁为 `cargo fmt --check`、严格 clippy、`cargo test --workspace`（448 passed），另有 7 个 Python 测量 helper 测试。

生产 ccrun/ACL/sudo/egress 全套验收、公开 provider 选择器、Codex colocated 支持均不在本阶段开放范围。
