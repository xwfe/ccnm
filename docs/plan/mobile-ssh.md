# 方案一：Tailscale + 手机 SSH

对应阶段：**P54**。共同约束见[移动端总纲](mobile-access.md)，进度见[状态账本](status.json)。本文只规定实施内容；规划时未连接手机、Agent 或 hpsrv。

## 1. 交付目标

从手机 SSH 客户端连接常在线 Agent Mac 的 Operator 登录身份，使用公共 ccnm CLI 管理 hpsrv 上的具名项目。无需新建移动应用、复制源码、把 AI 登录移到 hpsrv 或修改 ccnm 内核。

首版接入一个已注册 workspace、一个已配置 Agent instance、用户实际使用的手机。其他 workspace/provider 复用结构，但支持结论分别验收。

## 2. SSH 接入决策

**默认复用 Tailscale 网络 + 普通 SSH key 认证。** 在手机里填写 Agent 的实际 MagicDNS 名/IP、SSH 端口和 Operator 用户名。它们不是 hpsrv 的 `ccrun`，也不是 ccnm 配置中的 runtime SSH alias；alias 仅在配置它的机器上生效。

手机生成独立设备密钥，公钥经授权加入 Agent；不复制日常电脑私钥，不默认开启第三方客户端的云端密钥同步。核验 Agent 主机密钥后固定信任，主机密钥变化先调查，不设置跳过验证。关闭 Agent forwarding 与无关转发；OpenSSH 对转发风险的说明见[官方 ssh_config](https://man.openbsd.org/ssh_config)。第三方手机 App 不一定读 OpenSSH 配置文件，应记录其等效选项。

已有 Tailscale SSH 不能按普通 key 认证验收。先确认是谁在应答、允许哪些本地身份、是否允许 root；现有模式不符合目标时列出迁移动作并等待授权，不自动启用/关闭 `--ssh`，不安装第二个 Tailscale 变体。参见[Tailscale macOS 变体](https://tailscale.com/docs/concepts/macos-variants)。

不要求手机安装 CLI；Tailscale 官方文档说明 iOS/Android 没有 Tailscale CLI，手机操作应通过 App 与 SSH 客户端完成，见[CLI 文档](https://tailscale.com/docs/reference/tailscale-cli)。

## 3. 任务拆解

| 判据 | 实施任务 | 交付/通过条件 |
| --- | --- | --- |
| P54.1 | 只读核对角色、构建、账户、权限、网络、Controller 和工具链 | 一份脱敏基线；两端包含 P52 修复，Agent 可用，workspace 指向 hpsrv；未知项不得算通过 |
| P54.2 | 编写可照做的 SSH 接入与回退手册、设备 key 与网络授权清单 | 配置值现场填写，明确普通 SSH/Tailscale SSH 分支；不把静态示例当现网配置 |
| P54.3 | 仓库内检查与离线验证 | 文档门禁通过；新增 helper 必须有测试、只读预检、超时、脱敏与失败退出；不为这条路线强造脚本 |
| P54.4 | 授权后完成手机真实项目闭环 | 在蜂窝网络上从手机启动/接回/输入/审批，hpsrv 上改动、测试与属主证据一致；记录实际 provider |
| P54.5 | 验证掉线、权限拒绝、停止、撤权与恢复 | 下方矩阵逐项有证据；正式使用手册与支持矩阵更新，回退记录齐全 |

### P54.1：预检清单

核查 Agent 的 ccnm、tmux、官方 CLI 与登录状态、Controller 运行身份和 GUI 登录上下文。只检查登录是否有效，不读认证文件内容。锁屏、登出和重启是不同状态；不用“装了 LaunchAgent”证明冷启动后无人值守可用。

核查 hpsrv 实际 OS/架构、已装构建、`ccrun` UID/组/无 sudo/无高权限 socket、AI 凭据不可达、工作区权限与构建工具链。沿用[生产安全](../production-safety.md)与[运维](../operations.md)的判据，不用 SSH 成功替代 Runtime 身份审计。

分别确认手机 → Agent、Agent → Runtime 两条链。记录 Tailscale direct/relay 和代理/TUN 共存情况，不要求必须 direct；弱网允许慢，不允许错误地显示任务完成。不为手机入口设置全局代理旁路、出口节点、全网段路由或修改 Runtime egress。

在未占用的目标 workspace 做一次 `ccnm doctor`。**doctor/MCP probe 可能临时取写锁，不是无副作用健康检查**；不要轮询它监控正在工作的会话，也不要把忙碌时的 busy 当连接失败。

### P54.2：用户手册必须包含的最小操作

以下命令是现有 CLI 形状，示例参数必须替换为预检得到的 workspace、instance 和 ccnm session ID：

```bash
# 在手机 SSH 到 Agent 后运行。先查看，再明确启动。
ccnm ls
ccnm run my-project --agent claude-main --detached
ccnm status my-project --all

# 使用 status 核实的 ccnm session ID；不要填官方 CLI 的 thread/resume ID。
ccnm attach my-project --agent claude-main --session <ccnm-session-id>

# 需要结束任务时才执行；不是离开手机前的必做步骤。
ccnm stop my-project --agent claude-main --session <ccnm-session-id>
```

`--agent` 必须是现有配置支持的 instance；legacy 配置先按[使用说明](../usage.md)确认适用命令，不为了照抄示例重建登录。写全 `run`，避免 workspace 名与子命令简写冲突。

在手册中标明三种退出：tmux detach 只离开终端，官方 Agent `/exit` 会结束会话，`ccnm stop` 是控制面明确停止。从 ccnm 状态栏或当前 tmux 配置确认 detach 按键，不硬编码假设所有人都使用 `Ctrl-b d`。**禁止建议使用 Claude 的“后台会话”来保持任务。**

当前 `--print` 应在定义 workspace 的一侧发起；手机直接连 Agent 的此路线不把它当主入口，更不能为此让 `ccrun` 持回连 Agent 的 key。依据见[使用说明的非交互模式](../usage.md#非交互---print)。

## 4. 真机验收矩阵

故障注入限定到专用测试 workspace/会话，先具备另一路管理连接和恢复手段。不能在用户正在使用的任务上直接断网或停进程。

| 编号 | 场景 | 判定依据 |
| --- | --- | --- |
| SSH-01 | 关闭手机 Wi-Fi，用蜂窝网络连接 | Agent 身份/主机指纹匹配；无公网端口映射；目标 ccnm workspace 正确 |
| SSH-02 | 发送中文、多行和带引号的任务，人工审批 | 输入无意外拆分/重复执行；批准与拒绝都有效，不打开逃生开关避开审批 |
| SSH-03 | hpsrv 上读 → 改 → 构建/测试 → 独立核验 | 变更与命令确属 ccrun；保存预先约定的测试结果、Git diff，恢复测试改动 |
| SSH-04 | 手机锁屏/切后台，前台恢复后 attach | 相同 session ID、Agent 进程身份与 Runtime writer；没有第二 Agent；等待审批不被描述为卡死 |
| SSH-05 | Wi-Fi/蜂窝切换与强制关闭 SSH App | 记录是否断线及重连结果；不重放未确认的输入；旧 attach 客户端不无限残留 |
| SSH-06 | 桌面 detach → 手机 attach → 桌面 attach | 同一会话接力；两端同时在线只作观察，不宣称输入互斥 |
| SSH-07 | 仅切断测试会话的 Agent → Runtime MCP | 命令按契约收尾/失败/unknown；写锁不错误释放；不能按 SSH-04 的预期继续构建 |
| SSH-08 | 错误设备 key、错误身份、未授权设备、主机密钥不符 | 被对应层拒绝；不自动降级为 root、密码或跳过主机验证；记录真实认证模式 |
| SSH-09 | 精确 stop 后再次 stop，再请求新 writer | 幂等语义符合现有契约；确认收尾与写锁后才允许下一 writer，不手删锁 |
| SSH-10 | 撤销测试手机 key/网络许可，再尝试连接 | 新连接拒绝；已有连接单独核查并按清单终止，Agent 是否继续运行分别记录 |
| SSH-11 | Agent 锁屏、登录上下文失效/Runtime 不可用 | 已有会话与新建会话分别观察；失败信息可诊断，不自动重登录或改系统睡眠策略 |

实际睡眠、注销、重启只在获准维护窗口中测试；没有授权时可以用隔离故障替代调试，但必须保留相应真机场景未验，不升级为跨重启恢复承诺。SSH-04/05 各至少完成三轮，覆盖一次命令运行中和一次等待人工批准；记录持续时长，不规定手机必须后台保持 TCP。

针对真实项目选一个可回滚的小改动和明确测试命令。安装依赖、改 `.git`、联网、发布均服从已有 Runtime 策略；沙箱不允许时记录拒绝，不关闭沙箱只为拿到绿灯。首次验收不涉及 production deploy 或长期服务。

## 5. 交付物与回退

实施后在拟新增的 `docs/mobile-access.md` 写面向用户的简短操作，细节继续留在本计划及研究记录；更新[支持矩阵](../support-matrix.md)、[排错](../troubleshooting.md)与[运维](../operations.md)。证据建议保存为 `docs/research/mobile-ssh-<日期>.md`；文档创建前不要放指向它的失效链接。

回退只删除本轮有标识的公钥、授权规则或配置片段；先核对是否被他人修改，冲突则停止自动回退。不得删除现有 key、Controller、项目、工具链或全局 Tailscale 配置。独立列出测试 session 的停止/清理以及日用 session 的保留，不把两者混为一份“清空环境”。

## 6. 实施者接续提示

```text
执行 P54 前确认 P52/P53 已完成，现场两端含修复；本文件不是部署授权。
先做只读预检与候选变更清单，再按获准范围实施手机 SSH。
只复用现有 ccnm CLI、Controller 与 Runtime 身份，不新建 Agent 调度层。
把 SSH-01 至 SSH-11 的实际结果写入证据，手机操作需要用户实际完成时如实等待该证据。
没有真机、缺授权或发现安全门禁失败时记录 blocked，保留已完成离线成果。
完成 P54 后停止；不要顺手安装 ttyd、开启 Serve 或进入 P55/P56。
```
