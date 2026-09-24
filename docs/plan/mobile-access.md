# 移动端远程操作：实施总纲

制定日期：2026-09-24。用户选择两条路线：**手机 SSH** 与 **ttyd + Tailscale Serve 浏览器终端**。本文与两份分方案是交给后续实施者的计划，不是安装记录或已支持声明。唯一进度仍在 [status.json](status.json)，阶段验收编号见 [ROADMAP.md](ROADMAP.md)。

## 1. 目标与范围

出差时，用户用手机接入常在线的 Agent Mac，由现有 ccnm 受管会话操作家中 hpsrv 上的项目。手机不运行 ccnm、不持有 Agent 的 AI 登录文件；hpsrv 继续承担 Runtime，不改成 Linux Agent。

```text
手机 SSH 客户端 ── Tailscale + SSH ──────────┐
                                           ↓
手机浏览器 ── Tailscale HTTPS → Serve → ttyd → ccnm attach
                                           │
                            Agent Mac / Operator 登录身份
                            ccnm Controller → 官方 Agent
                                           │
                                    SSH stdio MCP
                                           ↓
                            hpsrv / ccrun / Linux Runtime
                            项目、Git、构建、测试、工具链
```

两条手机路径是**同一执行体系的两个终端入口**，不是两个 Agent、两份源码或两个独立写锁域。

| 路线 | 本次规划的能力 | 明确不做 |
| --- | --- | --- |
| [方案一：手机 SSH](mobile-ssh.md) | 查看项目、启动、精确接回、人工审批、查看输出、停止、故障救援 | 移动 App、凭据同步、自建 SSH/VPN、让 ccrun 回连 Agent |
| [方案二：浏览器终端](mobile-web-terminal.md) | 接回预先指定的现有会话、输入、审批、滚屏、断线重连 | Web 新建任务/Agent、任意命令接口、多租户、文件管理器、聊天式 UI、推送通知 |

浏览器首版故意不承担会话创建与管理：新建、切换绑定目标和显式停止走方案一。它是个人高信任终端，不是可分享给第三方的项目协作入口。

## 2. 核查基线与不可跳过的前提

2026-09-24 规划时读取本地 `main/2eeaf7b`，工作树与暂存区为空；版本文档为 v0.9.0，`current_task=P52`。本轮没有登录 hpsrv/fodelf、检查手机或替换任何已安装二进制。

| 已核查的仓库事实 | 对实施的约束 | 依据 |
| --- | --- | --- |
| 受管 Agent 的 Controller 使用 macOS GUI 登录上下文；Linux Runtime 已有历史证据 | 选择一台常在线 Mac 作 Agent；fodelf 只是候选，现场确认角色、账号与配置 | [架构](../architecture.md)、[支持矩阵](../support-matrix.md) |
| 交互会话由 Controller 创建在 `tmux -L ccnm` 中，attach 不负责创建 server | 只复用公共 ccnm 命令；不得从网页/SSH 直接启动官方 Agent 或另建 tmux server | [tmux 实现](../../crates/ccnm-core/src/tmux.rs)、[使用说明](../usage.md) |
| 精确操作使用 workspace + Agent instance + ccnm session ID | 不使用 Claude thread ID、Codex resume ID、模糊名称或“最新会话”作浏览器绑定 | [使用说明](../usage.md) |
| P52 尚未修复 C51-01：Runtime relay 子进程可能存活而写锁已释放 | 正式移动真机验收前必须完成 P52，并部署、核实含修复的两端构建 | [P51 审计](../research/2026-09-23-lifecycle-and-docs-audit.md) |
| CI 尚缺 Python/计划/协议完整门禁 C51-03 | 先把既有后续项登记为 P53，不用“现有 CI 绿”代替完整验证 | [P51 审计](../research/2026-09-23-lifecycle-and-docs-audit.md) |
| Runtime 后台命令绑定 MCP 连接；Machine API 当前主要是 print | 不承诺 MCP 断线后继续构建；两条终端入口不依赖扩充 RPC | [使用说明](../usage.md)、[公开协议](../protocol/README.md) |

规划和不连接真机的设计审查可以先做；阶段认领仍按顺序。临时关闭 `[runtime_mcp]` 只能降低已知风险，**不能替代 P52，也不能据此把移动交接验收标绿**。不启用历史 `codex_exec_server`，不顺带移植 Linux Controller。

## 3. 核心契约

### 身份与网络

手机进入 Agent 的 Operator 会话；Agent 上的官方登录仍由正常登录身份持有；hpsrv 的 `ccrun` 仅为入站执行身份。入口不向 Runtime 下发 AI 凭据、手机私钥或 `SSH_AUTH_SOCK`。不把新建同名 OS 账号当成能访问原 Controller/tmux 的办法，现场核对实际 UID 与 GUI 会话。

Tailscale 是部署依赖，不进入 ccnm-core。方案一默认是 **Tailscale 网络中的普通 SSH**，不等于启用 Tailscale SSH；若现网使用后者，必须单独审查其身份授权及 root 放行，不能绕开既有配置悄悄切换。[Tailscale macOS 变体说明](https://tailscale.com/docs/concepts/macos-variants)表明不同安装形态的 SSH server、CLI 和登录前运行能力不同，实施时记录具体变体。

手机只获得到 Agent 所需入口的访问，不因此获得到 hpsrv 的新管理权限。拒绝公网转发、Funnel、全 tailnet 放行和默认关闭主机密钥验证。现有 broad allow 必须检查：Tailscale grants 是许可并集，新增窄规则不会覆盖宽规则，见[官方 grants 语义](https://tailscale.com/docs/reference/syntax/grants)。

### 四种生命周期不得合并

| 事件 | 要求 | 不能声称 |
| --- | --- | --- |
| 手机切后台、锁屏、SSH/WebSocket 断开 | 终端可脱离；Agent 与 MCP 健康时会话继续，回到同一 ID | 手机连接永不断、输入自动可靠重放 |
| Agent → Runtime MCP 断线 | 按 ccnm 契约停止/收尾命令；结果不明保持 unknown/未安全交权 | dev server 或构建必然继续 |
| Agent/Runtime 重启、睡眠、账号退出 | 明确显示不可用，记录人工恢复前提 | 自动唤醒、登录前就绪、跨重启无损续跑 |
| 用户显式停止会话 | 核实目标、进程收尾与写锁；第二 writer 仅在安全交权后进入 | 关闭网页等于 stop；Agent 退出码 0 等于业务验收通过 |

手机断线后，用户需要查看结果再决定是否重发输入；终端没有 exactly-once 消息协议。UI 网络可用性、Agent 活跃、Runtime 可执行、项目验收通过必须分别记录。

### 安全边界

固定 `ccnm attach` 仅减少误入和注入入口，不是 Operator 沙箱。tmux 本身存在命令提示、新窗口等能力，见[官方使用说明](https://github.com/tmux/tmux/wiki/Getting-Started)。方案二的访问授权应等同信任该 Operator 终端；不承诺“只能碰一个 workspace”。Agent 本机 MCP 的边界也不会因加手机入口而收紧。

同一会话可有多个终端；这不等于多个 Agent writer。首版约定同一时刻只在一个终端输入，不自动踢掉桌面客户端；网页连接数限制不能冒充跨 SSH/Web 的输入互斥。

## 4. 任务顺序与交付

| 阶段 | 主要交付 | 开始条件 / 停止点 |
| --- | --- | --- |
| P52（既有） | 修复 relay 收尾与写权交接，macOS/Linux 相应证据 | 按原路线完成；不在本次文档任务中实现 |
| P53 | 将既有 C51-03 转成 CI/release 真实门禁 | 依赖 P52；不为验证发布而创建 tag、push 或 release |
| P54 | 手机 SSH 使用/部署手册、授权清单、hpsrv 项目闭环及故障证据 | 依赖 P53；部署和真实模型另行授权；完成后停止 |
| P55 | attach-only 小型适配脚本、受控目标配置、ttyd/LaunchAgent 模板、离线回归 | 依赖 P54；只生成与测试产物，不持久安装或开端口 |
| P56 | Serve 私网部署、手机浏览器验收、SSH/Web 接力、撤权与回退 | 依赖 P55；授权缺失则 blocked，不自动开始 |

P53 只是把审计中已有的优先后续项编号，不是移动功能额外发明的重构。具名项目生命周期验证纳入 P54/P56 的开发闭环；发布、生产上线和长期服务托管不因此获得授权。

### P53 的具体边界

检查 `.github/workflows/ci.yml` 与 `release.yml` 的实际构建顺序：中立客户端测试需要真实 debug 二进制时，先显式构建，不能误用缓存。接入 `check_plan`、`check_protocol`、Python 对应平台可执行的全套测试；平台限制须保留具名 skip，不能整包跳过。release 发布/上传 job 要依赖门禁成功，测试失败不得继续发布。验证“故意失败的门禁阻止下游”的配置/本地证据；未运行 GitHub runner 时如实保留待验，不伪造线上结论。

### 通用交付位置

现有计划继续留在 `docs/plan/`；正式通过后新建 `docs/mobile-access.md` 作为用户短手册，并更新使用、运维、排错、支持矩阵。小型适配与部署辅助建议集中在 `scripts/mobile/`，测试放 `tests/`；这些路径在本轮**尚未创建实现**。

不新增网络服务 crate、不修改 ccnm 的模型策略/权限逃生开关、不为了方便网页而扩展 Machine API。发现确实需要核心行为变更时，先记录最小缺口与独立验收，不在部署脚本里偷偷补一套会话管理器。

## 5. 授权和资源台账

| 动作 | 授权边界 |
| --- | --- |
| 写方案、仓库内脚本/模板、离线测试 | 可在用户授权的当前实施阶段内进行；本轮只有文档规划 |
| 远端只读检查 | 明确目标机器、账号、检查内容；不读取凭据正文 |
| 安装/替换 ccnm、ttyd，建立目录或 LaunchAgent | 必须明确机器、身份、路径、版本、保留/撤销方式 |
| 配置 SSH key、开启 SSH、改访问规则/HTTPS/Serve | 分项授权；不得复用旧阶段 sudo 或网络授权 |
| 运行真实模型、真项目写入、断网或重启 | 明确 workspace、费用/回合上限、故障范围及恢复责任 |

部署前记录配置摘要/哈希、已占用的端口和 Serve 路由、本轮资源标识；敏感备份留在所属机器的私有目录，不进 Git。回退先撤本轮入口，再停本轮 ttyd/attach 客户端，保留既有 ccnm 会话；逐项恢复本轮改动，禁止 `tailscale serve reset`、全局 kill-server、清空 authorized_keys 或整文件覆盖 tailnet 策略。

撤销手机访问不一定能终止已经建立的 SSH/WebSocket；必须把“新连接被拒”和“现有连接被切断”分开实测。切断入口客户端不自动停止正在工作的 Agent。发现凭据泄漏或越权时按安全事件处理，而不是套用普通 detach 流程。

## 6. 证据与完成定义

每阶段在 `docs/research/` 写带日期的脱敏记录，并在 status 对应 evidence 引用。记录实际 Agent/Runtime build/hash、OS/架构、账号角色、手机 OS、客户端/浏览器版本、Tailscale 变体/版本、workspace 与 ccnm session ID、授权范围、判据、结果和清理情况。截图只是辅助；会话 ID、Runtime 文件属主、独立测试结果和写锁状态才是主要事实。

首轮至少覆盖用户实际手机；没有 iOS/Android 两种真机，就按实测客户端声明支持，另一种保留未验，不能拿浏览器响应式模拟代替真机。Agent provider 也逐个登记，Claude 通过不自动证明 Codex 通过。

正式可用必须同时证明：手机能操作同一会话；项目变更和测试确在 hpsrv/ccrun；手机断线不新建 Agent；MCP 故障不假成功；未授权设备进不来；入口可撤销且不破坏原有开发会话。长期无人值守、自动唤醒、跨重启恢复均不在这份完成定义中。

## 7. 交给实施者的入口

```text
读取 AGENTS.md、docs/plan/README.md、status.json 和 ROADMAP 当前阶段，
再读 mobile-access.md 与当前分方案。先核 Git 和未提交修改。
按 current_task 每轮只做一个阶段：P52 → P53 → P54 → P55 → P56。
不要因用户选了移动方案就跳过 P52，也不要把计划文件存在当成验收。
方案一读 mobile-ssh.md；方案二读 mobile-web-terminal.md。
遇到缺少系统/网络/部署/模型授权，列出精确动作与回退并记录阻塞。
没有真实手机/机器的证据不能写 completed；不自动 push、发版或改其他仓库。
```

外部文档核查日期为 2026-09-24。后续安装时重新核对目标版本的 `--help` 与官方文档，不将在线 main 的能力当作用户机器已安装版本的能力。
