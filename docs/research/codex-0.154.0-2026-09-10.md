# Codex 0.154.0 重新测量：变了什么，ccnm 跟着改了什么（2026-09-10）

`codex-cli` 从实测过的 `0.153.4` 升到 `0.154.0`，适配器按设计拒绝启动。这是[支持矩阵](../support-matrix.md)那套五步流程的记录，第 5 步（逐条比对差异）就是这份文件。

结论先说：**JSONL 没有破**，解析器读新旧两份流都对；但 CLI 表面变了三处，其中一处必须改实现。

## 一、这次测的是什么

```text
python3 scripts/measure_codex.py tests/fixtures/codex-0.154.0 seven-tools \
    --model gpt-5.3-codex-spark
```

`inspect` 那半（version/help/exec-help/login status/features）不启动模型；`seven-tools` 那半是真跑一次，让模型只用 ccnm 的七个工具，在假 Runtime 工作树里改一个哨兵文件。

旧目录 `tests/fixtures/codex-0.153.4/` 原样保留，它仍然是回归基线——那些字节是解析器最初写出来时对着的东西。

**为什么带 `--model`**：见第四节，不带就跑不动。

## 二、CLI 表面的差异

### 1. 子命令与参数

- `mcp-server`（"Start Codex as an MCP server (stdio)"）**没了**。ccnm 不用它（走的是 `exec` + `-c mcp_servers.*`），所以不影响，但它说明这个版本动过命令表。
- 新增 `--worktree`："Run the session in a new managed Git worktree"。ccnm 不传它。

### 2. features 表

| 特性 | 0.153.4 | 0.154.0 |
| --- | --- | --- |
| `memories` | stable, **true** | stable, false |
| `realtime_conversation` | under development | **removed** |
| `unified_exec_tty` | 没有 | **stable, true** |
| `guardianv2.thread_context` | 没有 | under development |
| `reasoning_effort_override` | 没有 | under development |
| `worktrees` | 没有 | experimental |
| `windows_sandbox_service` | 没有 | under development |

**`unified_exec_tty` 是必须处理的那一条。** ccnm 的禁用列表里有 `unified_exec`，没有这个名字——它是同一类东西（一条不经七工具的执行路径），stable 且默认开着。旧列表拦不住新名字，这正是版本 pin 存在的理由，也正是"改个常量"会漏掉的东西。

**改法**：`DISABLED` 加 `unified_exec_tty`（采集脚本那份同名列表一起加），然后**重测**——否则 fixture 记的 argv 就不是 ccnm 实际会发的那条。本目录的 fixture 是加完之后重跑的。

### 3. 多了两条告警

```text
Under-development features enabled: code_mode_only. …
Code Mode is enabled in configuration, but model `gpt-5.3-codex-spark` does not
advertise Code Mode support. This may degrade model performance. …
```

它们以 `item.completed` + `type: "error"` 的形状出现在事件流里，**不是失败**——终止事件仍是 `turn.completed`，退出码 0。结果解析要认得这种"error 项但不终止"的事件，现有解析器已经如此（旧版本就有 warnings/permission denials 不算终止失败的测试）。

第二条值得单独记：`gpt-5.3-codex-spark` **不宣称支持 Code Mode**，而 ccnm 固定传 `--enable code_mode_only`。这一轮跑通了，但这是"模型和启动参数不完全匹配"的既有事实，不是保证。

## 三、没有变的

- **JSONL 能解析**：`AgentProvider::Codex.parse_result` 直接读这次的 `seven-tools.stdout`，`usage` 有值、`is_error` 为假。旧 fixture 的解析测试一条没改、全部照过。
- **七个工具都通**：`workspace_info`/`list_files`/`search_text`/`read_file`/`apply_patch`/`exec_command`/`read_output` 各自至少成功一次，Runtime 工作树里的哨兵被改成 `CCNM_RUNTIME_PATCHED_7319`。
- **隔离成立**：Agent 侧那个同名哨兵文件 `WRONG_AGENT_NODE_9520` 一字未动——模型是经 ccnm 的工具在远端干活，不是在自己盘上。
- `usage` 仍然有、`cost` 仍然没有（Codex 不报费用，ccnm 不补零）。

## 四、被迫加的一个实现改动：instance 级 `model`

跑这次测量时撞上：**默认模型的额度用尽**（提示 9/15 恢复），而 `gpt-5.3-codex-spark` 有额度。但 ccnm 当时**没有任何办法指定模型**——它用 `--ignore-user-config` 启动 Codex（刻意的，免得 Agent 上一个文件改掉实测过的行为），所以 CLI 配置文件里的 `model` 不生效。

于是加了一个可选字段，落在 **Agent 本机的 instance 注册表**：

```toml
[agents.codex-main]
provider = "codex"
profile_ref = "default"
model = "gpt-5.3-codex-spark"
```

- 仅 Codex：给 Claude instance 写会被配置校验拒绝（ccnm 不给 Claude 传模型）。
- 值要过命令行安全检查，空值拒绝。
- **不上 wire**：不进 `AgentIdentity`、不进 binding、不进会话记录。supervisor 启动前本来就要重读本机注册表解析 profile 目录，模型在同一处一起取。
- 不写就是 CLI 默认值——也就是所有旧 fixture 当初被测量时用的那个。

## 五、`all_tools_succeeded` 是 false，原因要看清楚

采集脚本的严格判据没过，因为 `apply_patch` 前四次都失败了。逐条看：

```text
1. {"patch": "*** Begin Patch\n*** Update File: probe.txt…"}  → missing field `files`
2. {"files":[{"path":…,"patch":"@@\n-…"}]}                     → CCNM_E_INVALID_ARGS: op is required
3. {"files":[{…,"op":"replace",…}]}                            → unknown variant `replace`,
                                                                  expected one of add/update/delete/move
4. {"files":[{…,"op":"update","patch":…}]}                     → op "update" needs at least one edit
5. 正确的形状                                                   → 成功
```

模型先试了 **Codex 自己的 `*** Begin Patch` 格式**，再一路猜，每次都被 ccnm 按名字拒绝，第五次写对。这是**工具契约在起作用**，不是版本回归：每条拒绝都说清了缺什么，没有一次是"看起来成功了其实没做"。

旧版本那次 `all_tools_succeeded` 是 true，说明当时的模型一次就写对了——所以这是**模型差异**，不是 0.154.0 的问题。判据没有为了变绿而放宽：它衡量的是"每次调用都一次成功"，那是模型属性；契约属性（七个工具都可达、参数错必被拒、终止事件正确）由 `tests/codex_measurements.rs` 里新加的四条测试单独钉住。

## 六、顺带修掉的采集脚本缺陷

原脚本把假 Runtime 建在系统临时目录里。macOS 的 `/tmp` 和 `/var` 都是**符号链接**，而 Runtime 的凭据检查见到祖先目录是 symlink 就判 `accessibility unknown`——不可豁免，`allow_unconfined_exec` 也救不了。表现出来却是一句跟符号链接毫无关系的话：

```text
ccnm: handshaking with MCP server failed: connection closed: initialize response
```

脚本现在对临时目录取 `resolve()`。**这条对真实部署同样成立**，已写进[运维手册](../operations.md)。

## 七、这次测量没有覆盖的

- **用 CLI 默认模型的那条**：默认模型没额度，所以这份 JSONL 是 spark 的。旧目录覆盖着默认模型的形状。
- interactive 模式与 reverse-interactive：旧目录里有 0.153.4 的观测，这一轮没重做。
- 这份测量本身**不代表 P7.3 的 Codex 验收**——那要跑一次 CLI ↔ Machine API 的 parity，见 status。
