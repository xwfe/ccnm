# Codex CLI 0.153.4 实测 fixture

2026-09-07 从本机真实官方 CLI 捕获。不是手写的 Codex 输出示例，也不是已完成的 provider 实现。

详见 `docs/research/codex-provider-probe-2026-09-07.md`，包括来源、脱敏规则、失败试验、重放命令和交互模式的阻断点。

- `auth-*`：官方 status 的 stderr/exit；synthetic-key 仅使用临时目录中的无效合成哨兵。
- `mcp-config-*`、`*disabled-tools.json`：配置覆盖与严格解析的失败边界。
- `exec-mcp`：默认 auto 审批被拒绝，但 turn.completed。
- `exec-seven-tools`：真实 ccnm MCP 七工具成功流程；`exec-seven-tools-isolated` 与 `seven-tools-isolated.json` 是附加 `code_mode_only` / `agents.enabled=false` 后的独立成功重放。
- `exec-native-patch`：原生 patch 被 sandbox 拒绝，不等于工具未暴露。
- `exec-registry-*`：运行时注册表经模型抄回的观察；不可替代完整权限验收。
- `exec-logged-out`：重试后 turn.failed；`exec-no-tools`：warning 后成功。

CLI help 文本只清除了行末空格；个人路径、UUID、请求标识、账号名及 key 展示已脱敏；工具 version/output_ref 为临时 fixture 标识。不要用当前期望覆盖此前 fixture。

`reverse-interactive/` 是后续本机 Agent → 远端 Runtime 的真实 TUI/tmux/SSH MCP 测量；仍不代表产品 Codex provider 已开放。
