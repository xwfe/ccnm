# Codex Provider：第一轮真实 CLI 测量

**状态：第二阶段的测量子阶段；尚未开放 Codex provider。** 不修改现有 Claude 使用方式，不部署两端二进制，不更改个人 Codex 配置，不复制订阅/OAuth 凭据。

## 测量对象与边界

- Codex CLI `0.153.4`，macOS arm64；二进制 SHA-256 记在 `tests/fixtures/codex-0.153.4/manifest.json`。
- ccnm 基线 `a2550f9`；开始时 422 个 Rust 测试通过。
- 真实官方 Codex CLI + 真实 `ccnm internal mcp-serve`。Agent cwd 与 Runtime root 是不同临时目录，各放一个内容不同的 `probe.txt`。
- MCP 使用本地 stdio，不是 SSH；Runtime 子进程用 `env -i`，只带固定 PATH、临时 HOME/XDG 目录和合成 USER。没有向它传递 Agent 认证环境或凭据。
- 临时项目明确开启 `allow_unconfined_exec`。这是 scratch 集成测试，**不是 ccrun/ACL/sudo/network 生产安全验收**。
- 没有测试两台机器、Controller/tmux、认证后的交互会话；不能据此宣布第二 provider 完成。

## 实际结果

### 定位、版本与认证

`codex --version` 输出 `codex-cli 0.153.4`。`codex login status` 的结果在 **stderr**：

| 场景 | exit | stdout | stderr |
|---|---:|---|---|
| 当前官方 CLI 已登录 | 0 | 空 | `Logged in using ChatGPT` |
| 空的临时 CODEX_HOME | 1 | 空 | `Not logged in` |

另用官方 `login --with-api-key` 在临时 CODEX_HOME 写入一个**无效合成哨兵**，仅检查到 `auth.json` 存在，没有打开文件内容。`login status` 仍返回已登录并展示脱敏 key。因此 auth status 只证明本地认证状态，不证明凭据联网有效；不得把它当作模型可用性探测。

### 非交互输出

实测使用 `exec --json --ephemeral --ignore-user-config --ignore-rules --skip-git-repo-check`，prompt 从 stdin 传入。

- 输出是 JSONL 事件流，不是 Claude 的单个结果 JSON。
- `thread.started` 带官方线程 ID；不能假设可由 ccnm 指定该 ID。
- 一次会话可有多个 `item.completed/agent_message`：前面可能是进度，最后才是答案。
- 成功会话也包含 `item.completed`、`item.type=error` 的启动或 skill 提示；单见 `error` 不能判会话失败。
- 真正认证失败的 fixture 记录多次 retry、WebSocket→HTTPS fallback，最终 `turn.failed` 和 exit 1。
- `turn.completed.usage` 实测包含 `input_tokens`、`cached_input_tokens`、`cache_write_input_tokens`、`output_tokens`、`reasoning_output_tokens`。没有 Claude 的费用和 API 耗时字段，未来适配器不能把缺失值伪装成测得的 0 美元/0 秒。

### MCP 权限与 Runtime 真相

只改一个权限值得到：

- `default_tools_approval_mode="auto"` + `approval_policy="never"`：三个 MCP 调用均被拒绝，即使 `workspace_info` 也不放行；会话仍 `turn.completed` / exit 0。
- 改成 `default_tools_approval_mode="approve"`：调用成功。

随后完整跑过七工具：`workspace_info → list_files → search_text → read_file → apply_patch → exec_command → read_output`。读取实际 version 后 patch；Runtime 文件变成 `CCNM_RUNTIME_PATCHED_7319`，Agent 同名文件仍为 `WRONG_AGENT_NODE_9520`。这证明测试中读写/命令的事实来源是 Runtime root，而不是 Agent cwd。

### 工具隔离不能靠猜

- `tools.disabled_tools` 和顶层 `disabled_tools` 在当前 `--strict-config` 下均被拒绝。
- 只禁用 `shell_tool` / `unified_exec` 等 feature，仍能触达嵌套的原生 `tools.apply_patch`。对临时 cwd 的一次写入被 **read-only sandbox** 拒绝；没有创建文件。这是 sandbox 拒绝，不能冒充“工具已移除”。
- 再加入 `features.code_mode.excluded_tool_namespaces=["functions","collaboration"]`：运行时测量返回 `typeof tools.apply_patch == "undefined"`，嵌套注册表剩 7 个 ccnm 工具和 `clock__curr_time`。七工具流程仍可成功。
- `code_mode_only` 加上述过滤仍向模型声明顶层 collaboration；额外显式设置 `agents.enabled=false` 后，模型报告的顶层函数只剩 exec/wait/用户输入/clock。**没有实际调用 spawn 验证**；注册表 JSON 是经模型抄回的运行时观察，不能等同于所有模式/版本的权限证明。
- `code_mode_only` 在本版本仍有 under-development 提示；不得无版本约束地作为未来稳定契约。

这些配置的含义还参考了[官方配置 schema](https://learn.chatgpt.com/docs/config-schema.json)和[配置参考](https://learn.chatgpt.com/docs/config-file/config-reference)，但结论以当前安装版本的实际输出为准。

## 交互模式的阻断点

当前 interactive CLI 不接受 `--ignore-user-config`。实测 `-c 'mcp_servers={}'` 不清空已有服务器，profile 也是叠加；`ignore_user_config=true` 配置键并没有隔离效果。因此不能复用 exec 的启动逻辑，也不能枚举一次服务器后就声称排除了个人 MCP。

候选方案是 Agent Node 上专用 `CODEX_HOME`，由用户直接通过官方 CLI 单独登录，不复制原凭据。[官方环境变量文档](https://learn.chatgpt.com/docs/config-file/environment-variables)说明该目录同时关联配置和认证等状态。**此方案已向用户询问，尚未确认或执行**；确认之后还须实测交互 UI、MCP 权限、项目上下文和 Controller/tmux 链路。

本轮没有新增 `AgentProvider::Codex`、public provider 配置、Agent Instance 或多 Agent 编排。Claude 行为快照与原配置不动。

## 重放与回归

```bash
# 不启动模型：采集版本/help/features/认证状态；输出目录必须不存在。
python3 scripts/measure_codex.py /tmp/ccnm-codex-inspect inspect

# 显式消耗现有官方 Codex 登录/额度；只操作本轮临时 fixture。
cargo build -p ccnm-cli
python3 scripts/measure_codex.py /tmp/ccnm-codex-seven seven-tools

# 不启动模型的 fixture 与脚本回归。
cargo test -p ccnm-core --test codex_measurements
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_measure_codex.py' -v
```

脚本拒绝覆盖输出目录、拒绝非 `0.153.4` 的版本、限制模型运行 120 秒并在超时后终止进程组。只使用现有官方 CLI 登录，不读认证文件；不会安装/更新 CLI、修改用户配置、连接用户配置里的其他 MCP，或把官方订阅凭据送往 Runtime。

已提交 fixture 脱敏了个人路径、UUID、API-key 展示、请求标识和账号名，保留事件形状、顺序、exit 与工具结果。不能为了让适配器通过而覆盖这些期望；新版 CLI 应增加独立版本目录重新测量。
