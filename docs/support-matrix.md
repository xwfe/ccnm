# 支持矩阵

本页只描述当前代码和现有证据。`docs/plan/status.json` 是阶段进度的唯一事实来源；历史真机记录不能替代当前 build 的重新验收。

## Provider 与 topology

| 配置 / 入口 | 当前状态 | 证据与限制 |
| --- | --- | --- |
| legacy Claude，remote SSH MCP，从 Runtime Node 发起 | 预发布支持 | 旧公开命令、Claude v1 wire 与 remote CLI golden 保持兼容；历史双机已真机跑通。当前 P3 build 尚未重新部署验收。 |
| legacy Claude，remote SSH MCP，从 Agent Node 发起 | 预发布支持 | `run` 先委托 Runtime 解析 workspace，`attach/status/result/stop` 继续在 Agent 本机管理已有 session；`--print` 仍需在 Runtime Node 执行。 |
| Claude Agent Instance，remote SSH MCP | 离线候选 | 默认 instance 与 `--agent`、print/interactive、doctor、精确 session、profile 隔离均有合成/真实进程测试；公共双机 dogfood 待 P3.5。 |
| Codex Agent Instance，remote SSH MCP | 离线候选 | 仅接受实测的 Codex CLI `0.153.4`；历史 internal 双机链路已真机验证，P3 公共入口只完成离线回归，尚未真机复验。 |
| Claude legacy colocated | 明确拒绝 | remote-only 启动参数已从 native 候选命令移除，但 installed Claude 尚未真实验收；本 build 在创建 session 前返回 `CCNM_E_NOT_READY`。 |
| Claude/Codex Agent Instance colocated | 明确拒绝 | 没有可信 Runtime credential boundary 和真实验收，不自动降级为 legacy/native。 |
| Codex legacy/internal protocol 2 | 兼容历史 fixture | 只用于保留已有内部测量与回归，不是新的公共配置入口。 |
| `hybrid-smb`、第三 Provider、Browser/Git 专用 MCP、多 Agent/worktree 编排 | 未实现 | 不属于 P3，不做隐式 fallback。 |

当前目标平台是 macOS。Linux Controller、Windows 和其他官方 CLI 版本均未验收。

## Agent Instance 入口

Runtime workspace 只保存 `{node, instance}` 引用；Provider 和 `profile_ref` 由 Agent Node 的 `[agents.*]` 解析。在 Runtime Node 发起时，以下命令共享 workspace 默认值或显式选择；Agent Node 的本机生命周期命令不重新查询 Runtime 默认值，跨 instance 管理请同时指定 `--agent` 和 `--session`：

```bash
ccnm doctor demo
ccnm run demo
ccnm run demo --agent codex-main
ccnm attach demo --agent codex-main --session <ccnm-session-id>
ccnm status demo --agent codex-main --session <ccnm-session-id>
ccnm result demo --agent codex-main --session <ccnm-session-id>
ccnm stop demo --agent codex-main --session <ccnm-session-id>
```

`--agent` 只能是同一 Agent Node 上已配置的 instance id，不能传 node、Provider、root、profile 路径或官方 CLI argv。legacy workspace 不接受该参数。

ccnm session id 是生命周期主键；Claude/Codex 自己的 thread/resume id 只作为结果元数据，两者不能混用。精确命令会同时校验 workspace、instance 和 session 记录。

## Runtime 单写 guard

每个真实 `internal mcp-serve` 在 Runtime 上持有工作树级独占 guard，直到整个 MCP server 结束：

- 普通目录按 canonical root 互斥；嵌套 root 和 symlink alias 配置拒绝；
- Git workspace 按 canonical `git-common-dir` 互斥，因此同一仓库的 worktree 也保守串行；
- 正常退出写入精确 `released` 状态并显式解锁；
- live owner 返回 busy；异常退出留下 `held <session> <workspace>`，状态为 unknown，不按时间自动接管；
- guard 覆盖 `exec_command` 和 `apply_patch` 所在的完整 MCP 生命周期，不只是某个工具调用或某个 Agent Node。

异常恢复必须由 Runtime 操作者完成：先按 session/status 和进程列表证明旧 supervisor、Agent、SSH MCP 及其子进程都已结束，再在 Runtime 的 `${XDG_STATE_HOME:-$HOME/.local/state}/ccnm/write-guards/` 中定位包含该 session id 的**单个** marker，备份后删除该文件。不要批量删除，也不要仅因时间过去就清理。删除前无法证明旧执行者结束时，保持 unknown 才是正确状态。

## P3 发布门禁结果

四条门禁的实测结果，证据见 [Codex 方向](research/p3-public-codex-2026-09-08.md) 与 [Claude 方向](research/p3-public-claude-2026-09-09.md)：

1. **通过（Ctrl-D 除外）**：当前 build 在授权双机上分别跑通 Claude/Codex 公共入口的 print、interactive、stop、detach/reattach、Controller 重启和链路失败。Ctrl-D 未取得证据——官方 CLI 对该键无响应，只用 `/exit` 覆盖了同一条自然退出路径，两者不等价。
2. **通过**：目标 Runtime 的专用执行身份能正常使用项目，但读不到任何已知 Agent 凭据和 SSH 私有状态，没有 sudo/admin，特权 socket 不可写。
3. **未验证**：本轮只做了 transport 进程故障注入，不等同物理断网，egress/网络策略没有逐项验证。**因此这里不声明任何 egress 边界**；需要这种保证的场景，由 OS 和网络层自己落实，不要拿本项目的诊断输出当依据。
4. **通过**：不复制现有订阅凭据，不把 `CODEX_HOME`、`CLAUDE_CONFIG_DIR` 或 profile 路径发给 Runtime。

双机验收用的临时账号、组、公钥、SSH 准入和测试目录已全部清理归零，无无法清理项。

离线门禁通过不等于 production READY，也不授权部署、替换正在运行的 Controller、创建账号或修改 ACL/防火墙——这些动作每次都要单独授权。
