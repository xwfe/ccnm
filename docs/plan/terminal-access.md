# 外部终端接入：范围收敛与旧计划撤销

决策日期：2026-09-28。核对基线：本地 `main/766b70a`。本文记录用户对接入方式的调整，不是新的移动端实施路线，也不是 PocketShell 的安装或兼容性验收报告。阶段进度以 [status.json](status.json) 为准。

## 决定

**直接在 PocketShell 等第三方工具提供的远端交互终端中运行现有 ccnm CLI。** ccnm 不再单独开发或维护手机 SSH 接入工程、浏览器终端、attach-only helper、ttyd/Serve 部署模板或移动端验收矩阵；也不改成开发 PocketShell 专用适配器。

PocketShell 是用户选择的外部入口，不是 ccnm 的必装依赖。终端界面、键盘、渲染、SSH/PTY 连接、认证及网络接入由所选工具和部署环境负责；ccnm 继续只管 Agent 执行、Runtime 工具、会话事实和写权限。不要求手机必须启用 Tailscale，也不把其他 VPN、隧道或公网入口实现进 ccnm。选择客户端不自动证明现网可达或安全。

## 使用边界

推荐的角色关系保持不变：第三方终端进入 **Agent Node 的 Operator shell**，在那里执行 `ccnm run/attach/status/stop`，由既有 Controller 管官方 Agent，项目工具仍经 SSH stdio MCP 在 **hpsrv / Runtime Executor（ccrun）** 上执行。界面在手机或浏览器中，不表示 ccnm 和 AI 登录应迁到手机。

外部工具可以直接提供目标机器上的终端，也可以经它自己的后端建立 SSH 连接；以命令实际落在哪台机器、哪个 UID 为准，不假定某一 PocketShell 版本的内部拓扑。进入的必须是能够执行自定义命令的交互终端，而不是只能启动原生 Claude 的固定入口。必要的客户端设置在客户端侧完成，不因此扩充 ccnm 协议。

也可按既有配置从 Runtime Node 的 **Operator** 发起；不得为此让低权限 `ccrun` 持有主动回连 Agent 的私钥。当前受管 Agent 仍以 macOS 为支持范围，hpsrv 的 Linux Runtime 不因此变成 Linux Controller。命令、会话 ID 和断线边界见[使用说明](../usage.md#通过第三方终端使用)。

不再套用旧网页方案的“只允许 attach”限制：用户可以使用正常 CLI 查看、启动、接回及停止会话。不要另起裸 Claude/Codex、另建一套 ccnm state 或 tmux server 来绕开现有受管会话。可写终端按相应 Operator 权限保护，第三方入口不会替代 Runtime 身份、网络授权或 OS 隔离。

## 撤销映射

| 原阶段 | 本次处理 | 仍应保留的内容 |
| --- | --- | --- |
| P54：手机 SSH 操作 hpsrv Runtime | 未实施，撤销独立阶段 | 普通 CLI 用法和真实项目日用检查，不设手机专属门禁 |
| P55：attach-only 浏览器终端离线适配 | 未实施，撤销 | 不实现 helper、目标绑定、Web 模板及其测试 |
| P56：Serve 私网部署与手机联合验收 | 未实施，撤销 | 外部入口由用户另行部署，不作为 ccnm 产品任务 |

三阶段从活动 `tasks` 与 ROADMAP 验收队列移除，**不标成 completed，也不保留成永远等待的 pending/blocked**。P54–P56 编号退役，不分配给新任务；确需新核心阶段时从 P57 起另定范围。schema 和检查器不为这次撤销新增状态。

旧三份 `mobile-*.md` 计划从当前树删除，原文保留在 Git 历史（规划提交 `ae06327`）。根目录若有用户自行下载的 `ccnm-mobile-handoff.md`，它是旧方案快照，不是当前任务入口；本轮不修改、暂存或提交该用户文件。

## 保留的完成进度

P52 的收尾/交权修复与 P53 的 CI 门禁不依赖移动方案，保持原有 completed 和证据不变。

- P52 / C51-01：macOS 与 hpsrv/ccrun 的 Linux 正反回归已有记录；同组后代收尾已修，脱离进程组的后代、ccnm 自身被强杀等边界仍在。[证据](../research/2026-09-25-p52-relay-group-cleanup.md)
- P53 / C51-03：CI 两个平台的门禁已有 GitHub runner 成功记录；发布工作流已接入相同门禁，但首次线上 release 执行仍待下一次授权发版观察。[证据](../research/2026-09-25-p53-ci-gates.md)

移除尚未开始的三个阶段后，保留队列没有未完成阶段，因此 `current_task = null`。这仅表示**当前没有排定的实施阶段**，不代表所有能力、客户端或完整项目交付都已验收。历史真机修复测试也不代表用户机器已经安装了对应版本。

## 后续接续，不另造移动项目

后续优先按已有开发流程做具名真实项目日用：先在获准范围确认实际 Agent/Runtime、构建与执行身份，再用普通 CLI 在 hpsrv 完成一个可回滚的读改测闭环，并核对 detach/重新 attach、停止及产物。任何合适的终端都可参与，不要求为 PocketShell 新建服务、先完成整套手机测试或关闭安全门禁。

输入法、渲染和客户端连接问题先在外部终端处理；在普通终端也能复现的 ccnm 会话、输出、命令收尾或写权问题才列为核心缺陷。保留[支持矩阵](../support-matrix.md)与[生命周期矩阵](../project-lifecycle.md)已有的缺口：Machine API 交互/分页/busy、MCP 断线与强杀边界、跨身份清理、真实模型覆盖等。按实际影响和复现证据选择下一项，不因为取消移动计划就自动扩展 API 或重开封存的 exec-server。

部署/替换二进制、SSH/网络/权限变更、真实模型额度、真项目写入和发布仍需各自授权。本次仅整理仓库文档与进度，不连接或修改 hpsrv、fodelf、PocketShell，不新增第三方客户端已通过声明。

## 本次文档验证

`python3 -B scripts/check_plan.py` 通过，保留队列为 53/53（含历史基线），下一阶段为 `None`；`test_check_plan.py` 14 项通过；`git diff --check` 通过。原有已完成任务、验收证据和授权记录保留，撤销阶段不计作完成。额外协议检查调用被工具安全检查拦截，没有执行结果；本轮没有修改协议或产品代码，也没有重跑远端、真实模型或发布流程。
