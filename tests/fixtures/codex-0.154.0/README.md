# codex-cli 0.154.0：测量到一半，**不能当作已测版本**

这是 [支持矩阵](../../../docs/support-matrix.md)里那套重新测量流程的第 2 步，**只完成了不花额度的那一半**。

**适配器的 pin 仍然是 `0.153.4`，故意没动。** 拿这个目录去改 `provider/codex/mod.rs` 的 `VERSION` 就是把一次没做完的测量当成做完了——JSONL 那半没测，而 JSONL 正是解析结果的依据。

## measured：CLI 表面（`inspect`，不启动模型）

```text
version.json          --version
help.json             --help
exec-help.json        exec --help
auth-help.json        login status --help
auth-current.json     login status（专用 CODEX_HOME）
auth-empty-home.json  login status（空 HOME，对照）
features.json         features list
```

里面没有凭据：两个 auth 文件只有 `Logged in using ChatGPT` / `Not logged in` 两行状态。

## not measured：JSONL 那半（`seven-tools`）

`seven-tools.stdout` / `seven-tools.json` 是**失败**的那次留下来的现场，不是 fixture。终止事件是 `turn.failed`，七个工具一个都没跑到，原因在事件流里：

```text
You've hit your usage limit. … try again at Sep 15th, 2026 10:29 AM.
```

ccnm 给 Codex 用的是专用 home（`~/.config/ccnm/agents/codex`），**从不复制 `~/.codex`**，所以个人那份的额度不影响这里——要接着测，得由使用者在那个 home 里登录一个有额度的账号：

```bash
CODEX_HOME=~/.config/ccnm/agents/codex codex login
```

## 这次尝试顺带修掉的一个真问题

采集脚本原本把假 Runtime 建在系统临时目录里。macOS 的 `/tmp` 和 `/var` 都是**符号链接**，而 Runtime 的凭据检查见到祖先目录是 symlink 就判 `accessibility unknown`——那是不可豁免的失败，`allow_unconfined_exec` 也救不了，表现为 MCP 握手在 initialize 就断开：

```text
ccnm: handshaking with MCP server failed: connection closed: initialize response
```

`scripts/measure_codex.py` 现在对临时目录取 `resolve()`。**这条对真实部署同样成立**：Runtime 执行身份的 home 只要有一层祖先是符号链接，会话就起不来。

## 免费那半已经看出来的差异（对比 `../codex-0.153.4/`）

- `mcp-server` 子命令没了；新增 `--worktree`（"Run the session in a new managed Git worktree"）。
- features 表：`memories` 默认 `true` → `false`；`realtime_conversation` 标为 `removed`；新增 `guardianv2.thread_context`、`reasoning_effort_override`、`worktrees`、`windows_sandbox_service`，以及 **`unified_exec_tty`（stable，默认 true）**。
- **`unified_exec_tty` 是要决定的那一条**：ccnm 的 `DISABLED` 关的是 `unified_exec`，没有这个名字，所以 0.154.0 上它默认开着而 ccnm 不知道。
- `--enable code_mode_only` 现在会让 CLI 发一条 `item.completed / type=error` 的告警（"Under-development features enabled"），0.153.4 上没有。这条只是告警，不是失败，但结果解析要认得它。

旧目录 `codex-0.153.4/` 原样保留，它是回归基线；比对差异（流程第 5 步）要等 JSONL 那半测到之后才能做完。
