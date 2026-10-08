# 基线复现与真实项目验收：P57 / P62

本文件与[总纲](core-hardening.md)配套。P57 是无真实模型的离线准备，P62 才承担新一轮真实环境结论；两者不可互相替代。所有部署、远端系统动作、项目写入和额度仍需具体授权。

## 1. P57：建立可接续的基线，而不是重新实现功能

### 1.1 必读与复用

先读 [AGENTS.md](../../AGENTS.md)、计划入口、status/ROADMAP、[支持矩阵](../support-matrix.md)、[生命周期](../project-lifecycle.md)和本轮三份分方案。核对 Git HEAD、全部未提交与已暂存差异。

复用仓库已有设施：`scripts/ci_gates.py` 自建 CLI 和执行中立测试；`tests/mcp_client.py`、`tests/test_blackbox_client.py`、`tests/test_execution_backend.py`、`tests/test_remote_workspace_mcp.py`；真实链路准备参考 `scripts/p7_parity_check.py`、`scripts/p11_matrix_check.py`、`scripts/p12_dogfood_check.py`，但不自动运行这些脚本的 SSH/模型分支。

不得为基线新建另一套任务管理器、公共 RPC harness 或网络测试服务。确需缺口探针时放在 `docs/research/probes/`，无模型、无真实登录；对应修复阶段再把反证移入正式测试。基线阶段不要把故意失败的产品断言无条件接进主分支 CI，也不要加永久 skip/xfail 让门禁看起来变绿。

### 1.2 离线任务

| 判据 | 实施任务 | 完成依据 |
| --- | --- | --- |
| P57.1 | 固定源码、工具链、构建与测试基线 | 记录 commit/OS/架构/工具版本、实际命令与退出码；P52/P53 不重新认领 |
| P57.2 | 对总纲 CTRL/OUT/AUTH/CLEAN 缺口做无额度复现 | 每项给“复现/已证否/静态待验证”，附方法与原始结果；不能把搜索命中算运行证据 |
| P57.3 | 建立普通项目闭环与失败样例 | 至少一个小型 Rust 样例和一个使用现有工具链的 TS/Node 样例，具有可回滚改动、确定测试和大输出；不得为此创建真实用户项目或安装全局依赖 |
| P57.4 | 形成 P62 的目标清单、权限清单与验收记录模板 | Agent/Runtime/实例/workspace/预算待现场确认；不预填通过，不夹带任何凭据 |
| P57.5 | 提交基线记录并校对后续范围 | 路径/引用/计划校验通过；为 P58 交接具名反证和已有通过行为，修订证伪的计划假设 |

执行基线至少包含 Rust fmt、clippy、全量测试与 `python3 -B scripts/ci_gates.py`；以实际依赖/网络条件记录 `--locked`、`--offline` 使用情况，不能把无法构建跳过。此阶段不增加功能，故障若阻断普通基线须先记录，不放宽检查来继续。

### 1.3 复现设计

**RPC 控制窗口**：用已有 runner/fake transport 与可控 barrier 让 A 保持运行、B 停在准备边界，再请求停止 B。记录实际调用目标；当前尚无提前 ID 时以反证说明，而不是直接从注释宣布“会停错”。随后对同记录 stop/finish 并发、重复 start_key、损坏/中断记录、配置变更、非法句柄做具名探针。保存运行次数、进程与文件清理结果；全程不调用真实 Agent 登录。

**输出通道**：分别制造成功可解析 stdout、不可解析 stdout、stderr 与数 MiB 的早/中/晚 marker。对照 Agent 源文件、RunReport、RPC text/output、参考客户端；确定从哪一步丢失。额外验证 `max_bytes`、非空 cursor、中文边界，不能只测试 fixture 中声明的输出。

**写权与清理**：复用外部 MCP 中立客户端创建隔离 coding 会话，核对同 state/common-dir 拒绝另一 writer；强杀探针只限本轮已知 PID/目录，有退出清理。模拟 Operator 与 Executor 不同 state 的 purge 路由，并记录“路径路由测试，不是不同 UID 真机授权证明”。不得在日用 state 上删 marker 或故障注入。

样例不是新产品：优先复用已有 fixture；需要新项目文件时固定依赖与命令，离线可重复，中文/大输出不依赖随机数据。失败与修复样例要能对比；最终 Git diff、文件归属、独立测试结果分别记录。

### 1.4 P57 停止点

交付建议记录 `docs/research/2026-xx-xx-p57-core-baseline.md`，实施时使用实际日期，并将实际文件写入 status evidence。每个反证写来源 commit、操作、观察、是否清理、未覆盖范围。外部模型额度为零、无远端部署是本阶段范围，不等于双机验收完成。

P57 完成后停止，下一轮按[会话控制方案](core-session-control.md)做 P58；不要顺手实施六个阶段，不改用户的 SSH/Tailscale/终端配置。

## 2. P62：现有支持范围内的真实联合验收

### 2.1 进场条件与目标

P58–P61 已有本地和适用 CI 证据；当前分支无混入修改；候选构建、两端版本/摘要、官方 CLI 实际版本和合法登录方式确定。Agent 仍是受支持 macOS 机器，候选 fodelf 只在用户批准后使用；Runtime 是 hpsrv Linux 的专用执行身份。不得把“旧阶段在这台机器验过”当成本轮基线。

Codex 当前受管 pin 以代码和支持矩阵为准，不直接采用机器上最新安装版本。若原 pin 不可用或官方 CLI 行为已变，先核对官方一手文档及当前二进制探针，单列兼容性变更和模型授权；不能临时增加 allowlist 或重新录制 golden 充当通过。P62 不默认升级 Provider、复制认证文件或新增模型供应商。

使用一个预先批准的普通项目及可回滚任务。若没有适合的真实项目，用户可授权在 hpsrv 建测试 workspace；必须说明它仍不是生产项目交付证据。终端可以是桌面 SSH、PocketShell 或其他客户端，**不把某手机/App 版本作为产品完成门槛**。

### 2.2 分项授权清单

| 授权项 | 必须写清楚的内容 |
| --- | --- |
| 远端只读核查 | 机器、登录身份、检查范围；不读认证正文；现有 SSH alias 与主机身份先核对 |
| 临时测试与真项目写入 | 精确目录/workspace、owner、基线 commit、允许改动、回退和保留物 |
| 部署候选二进制 | 两台机器各自的路径、版本/hash、替换或并存、备份与恢复；不改变其他正在使用的配置 |
| 真实模型 | Provider/实例、工作区、允许的任务与额度/次数上限；预算未明确前不调用 |
| 故障注入 | 精确测试进程/连接、断线种类、恢复通道；不含全局断网、注销或重启生产机器 |
| 清理/维护 | 本轮清单和 token、允许删除资源；不删项目/凭据/活会话，不继承旧 root/sudo 授权 |
| 推送、tag、release | 各自单独批准；打包与测试不等于获得公开发布权 |

默认只读核对后提交候选动作清单。获得某一项不代表其他项获准；权限不足时记录具体 blocker，不要求用户授予“全部权限”来绕开部署设计。

### 2.3 必验组合与行为

以 **macOS Agent/Host → hpsrv Linux Runtime** 为本轮真机目标；macOS Runtime 的代码回归/CI 仍须保持。Windows、Linux Agent、未跑的 CPU 架构或发行版继续标未验。

| 编号 | 入口与消费者 | 必须证明 |
| --- | --- | --- |
| REAL-01 | Managed Claude | 普通终端启动/精确 attach/审批/停止；项目操作确在 hpsrv，手机或桌面断开不等同 MCP 断开 |
| REAL-02 | Managed Codex | 使用受支持配置完成相同闭环；不以 Claude 成功代替；不使用封存 exec-server |
| REAL-03 | 外部 Claude Code + Remote Workspace MCP | read/coding 权限、拒绝分支与同 state 写互斥；不依赖 ccnm Controller 管理外部客户端 |
| REAL-04 | 外部 Codex + Remote Workspace MCP | 工具可见性、真实工具调用、禁止旁路改 Agent 私有项目的验证；这是单独的 Host 组合证据 |
| REAL-05 | Machine API + 仓库可独立运行的 Python 客户端 | 两 Provider 各一轮 start/status/result/stop；精确目标、幂等、完整分页、busy 和 unknown 处理，与人类 CLI 比对 |

若某组合目前明确不受支持，先在支持矩阵/计划登记实际不支持原因并走范围决策，不使用跳过行后照常完成的做法。不存在用于“通过测试”的通用降级开关。外部 MCP Host 的本机权限由外部配置管理，不能把 Runtime 沙箱解释成也隔离了 Host 本机工具。

每个适用组合以最小模型回合证明关键链路；大量边界仍用零额度中立客户端，不为凑计数反复调用模型。额外 skills/MCP 只临时启用具名的无秘密测试服务，验证 P48 的加载、P49 Runtime relay、P50 Agent relay 的实际调用身份与长结果；Agent HTTP 服务成功不替代 Runtime stdio 服务通过。

### 2.4 每轮项目闭环

记录基线、预期改动和明确测试命令 → 读取/搜索 → 最小修改 → 构建/单测（需要的项目再加端到端测试）→ 独立核对 diff/测试/Runtime UID → 读取完整报告 → 精确停止 → 核对写权与残留 → 按清单清理/回退。

必须有一次测试失败再修复，证明模型看到的是同一棵实际构建的源码；至少一次等待审批中与命令运行中的 detach/reattach。日志可能含敏感项目内容，记录仅保留必要脱敏证据。Git commit、下载依赖或非生产部署必须受原项目策略和专项授权约束，不通过关闭沙箱获得成功。

失败矩阵至少包括：重复 key、同工作区并发、配置/版本不符、Provider 不可用、Agent→Runtime MCP 断开、RPC 客户端丢响应、Agent/监督进程失联、held/abandoned、分页源头丢失、cleanup 部分失败。检查真实 effect/unknown，不能自动重放不明副作用。对未知/脱组后代保持拒绝算边界正确，不算“自动恢复已实现”。

### 2.5 候选包与发布边界

复用现有 `scripts/dist.sh`、`scripts/dist-linux.sh` 与 CI/release 门禁，核对当前目标平台的包、校验和、二进制版本、安装目录和 smoke；先读仓库实际脚本，不猜产物名称。macOS 安装沿用新文件加 rename 的方式，不覆盖已执行 Mach-O。目标机器须实际运行候选产物，不能以源码测试冒充发布件安装成功。

P62 的完成条件是候选包与具名真机结果，不要求为了阶段勾选擅自打 tag。P53 中“release workflow 尚未在线执行”的限制，在实际批准发版前继续保留；下一次发布应查看线上 gate、产物与失败路径，而不是把本地验证改写为 release 成功。若用户在 P62 授权公开发版，再记录真实 release 结果，否则仅交付候选包/发布清单，不标已发布。

P62.1 取得并核对分项授权和现场基线；P62.2 跑两 Provider 受管闭环；P62.3 跑外部 MCP 与真实 Machine API 客户端；P62.4 验证故障、写权、跨身份清理及边界；P62.5 验收候选产物和升级/回退；P62.6 同步支持矩阵、用户手册与状态，并完成资源归零或明确保留。

### 2.6 证据模板

```text
判据 / 入口 / 实际消费者：
源码 commit / 构建摘要 / 发布件或临时构建：
Agent/Runtime OS、架构、实际 UID 与实例、已核实的版本：
workspace / canonical 资源 / state 域 / public 与 managed session 映射：
授权范围与预算（无凭据）：
执行命令/步骤、实际退出码/协议响应、文件与进程证据：
测试及业务验收（分开填写）：
失败/unknown/拒绝分支及是否有旁路副作用：
保留的产物 / 本轮资源 / 清理与回退结果：
未覆盖平台、Provider、网络/强杀边界：
```

未运行写“未运行及原因”，不能填“理论通过”。授权不足、无法获得双机/真实模型证据、预算用尽或故障恢复不明时，按对应 P62 判据记录 blocked；离线成果和已有历史阶段不倒退、不伪装成新增真机结论。
