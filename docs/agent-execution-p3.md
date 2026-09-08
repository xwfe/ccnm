# 单 Agent 公共执行契约（P3）

P3 把已配置的 Agent Instance 接入既有 Controller/session/SSH MCP，不增加 Planner、Router、多 Agent、worktree 调度或 Machine API。实现与实时进度以 `docs/plan/status.json` 为准。

## 公共选择与身份

- instance workspace 的默认选择来自 Runtime 权威配置 `workspaces.<name>.agent`。公共命令用 `--agent <instance-id>` 覆盖时，只替换同一 `agent.node` 上的 instance；不接受 node、provider、路径、root 或原始 CLI argv。legacy workspace 不接受 `--agent`，原 Claude 默认行为和字段不变。
- `run`、`doctor`、`attach`、`status`、`result`、`stop` 使用同一受限 instance id。Agent-only 配置发起 `run`、`doctor`、MCP probe 时先把 `--agent` 转给 Runtime，由 Runtime 绑定 workspace/root/node；已有 session 的 `attach/status/result/stop` 保持在 Agent 本机，显式 `--agent` 由 Agent registry 校验，避免 Runtime 断线破坏旧 Claude 本地终端管理行为。稳定自动化再带精确 `--session`。
- Runtime 发出的 v3 请求只含 workspace、root、runtime_node 和 `InstanceRef`。Agent 解析后形成完整 `AgentIdentity` 并写入 session；MCP 初始化把 `WorkspaceBinding` 送回 Runtime。Runtime 必须用自己的配置重算 root 和引用，不能信任调用方覆盖。
- private profile path 只在 Agent 进程内解析。Controller 和 supervisor 都按 session identity 从 Agent 本地配置重新解析并比较，随后才把路径交给 Provider；路径不进入 Runtime payload、公开报告或错误。Codex default 仍为既有专用 HOME。
- instance session 使用内部协议 3。报告携带公开 identity，旧 peer 拒绝 v3；legacy Claude v1 和内部 Codex v2 保持兼容。不同 identity/provider/root 的活动 session 不复用、不自动替换；先显式 stop。

## 精确 session 与生命周期

- ccnm session ID 是 ccnm 生命周期主键；provider thread/resume ID 是结果中的独立字段，二者不等价。
- `attach/status/result/stop --session <ccnm-id>` 必须验证记录属于所给 workspace 和 Agent 选择。不给 session 时保留现有 workspace 行为；`result` 的“最近一次”仅为兼容便利，不作为稳定机器接口。
- status 区分 `starting/running/completed/failed/stopping/unknown`。terminal outcome 优先于 PID/tmux；找不到 outcome 且不能证明受管进程仍活着时是 unknown，不伪装 failed。启动在 session 落盘后失败也要写 terminal failure。
- stop 幂等，但只有确认 supervisor/tmux 结束并出现 terminal outcome 后才能报告 stopped。CLI 断开不等于 Agent 或 MCP 恢复；attach 只连接现有终端，不等于 provider resume。

## Runtime 单写者

- 所有带 `apply_patch`/`exec_command` 的 MCP server 在 Runtime 初始化时获取工作树级独占 guard，guard 由 Runtime 自己的配置、规范化路径和 session identity 决定，调用方不能指定锁文件。
- symlink/路径别名和配置中的嵌套 workspace 在 Runtime 校验后拒绝；Git workspace 以 `git rev-parse --git-common-dir` 的规范路径作为 guard key，因此共享 common git dir 的 worktree 保守互斥。普通目录以 canonical root 为 key。
- guard 文件位于 Runtime ccnm state，使用内核文件锁原子竞争。进程存活时不按超时转让。正常 shutdown 精确标记 released 并显式 unlock；进程异常消失后残留 `held <session> <workspace>` 视为 unknown，不能自动转让；部分/损坏状态也 fail-closed。P3 不承诺任意 shell 的 exactly-once、事务回滚或自动修复 stale guard。
- 同一 MCP 进程内的多次工具调用共用 guard，不重复获取。不同 Agent Node、CLI 或后续 RPC 只要进入同一 `internal mcp-serve` 都经过同一 Runtime guard；只在协调器保存 lease 不算隔离。

## topology、验证与发布边界

- Remote SSH MCP 支持 Claude/Codex print 与 interactive。Codex colocated 继续拒绝。
- Claude legacy colocated 的候选启动命令已移除 remote-only `--tools ""`、`--mcp-config`/strict 限制，但真实 installed Claude 尚未验证；公共入口因此在创建 session 前明确拒绝。instance colocated 仍不开放。
- P3 离线门禁之外，公共双机 dogfood、Ctrl-D/stop、detach/reattach、Controller 重启和链路失败需要真实节点证据。生产 READY 还需要专用执行身份验证 workspace 可用、Agent 凭据/SSH 私有状态不可访问、无 sudo/admin/特权 socket，并逐项说明网络策略。
- 当前用户没有针对创建账号、ACL/firewall、登录或部署替换的明确授权。先完成不改变系统的实现与 fixture；缺失的 P3.5 证据必须标记 blocked，不能拿 scratch/unconfined 或历史内部入口冒充生产验收。

公开配置/CLI 只写已实现语法。部署或替换正在使用的 ccnm/Controller、真实模型调用及系统安全变更均需针对该动作的明确授权；P3 代码完成不自动授权这些操作。
