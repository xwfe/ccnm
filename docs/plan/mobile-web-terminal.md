# 方案二：ttyd + Tailscale Serve 浏览器终端

对应阶段：**P55（离线适配）→ P56（授权部署与真机验收）**。依赖[方案一 P54](mobile-ssh.md)完成，SSH 保留为管理和救援入口。共同约束见[总纲](mobile-access.md)，状态以 [status.json](status.json) 为准。

## 1. 首版结构和非目标

```text
手机浏览器 / Tailscale 已连接
          │ HTTPS + WebSocket
          ▼
Agent Mac 上的 Tailscale Serve（仅 tailnet）
          │ HTTP，127.0.0.1 上的专用端口
          ▼
ttyd（Operator 身份、独立 LaunchAgent）
          │ 每个连接只产生一个 attach 客户端
          ▼
固定配置的 attach helper
          │ argv：ccnm attach <workspace> --agent <instance> --session <id>
          ▼
ccnm 已有受管会话 → 既有 SSH MCP → hpsrv/ccrun
```

不在 hpsrv 上启动 ttyd 来迫使 `ccrun` 回连 Agent，不开放 ccnm 内部 RPC 到网络。网页不新建/调度 Agent，不接受任意 shell、命令参数、路径或模型配置。新会话通过手机 SSH 创建；Operator 更新目标绑定后网页才接入。

先支持一个入口绑定一个 workspace 的**一个明确会话**。不做自动发现所有工作区、动态路由、多租户、文件上传下载或项目预览代理。需要切换时经 SSH 更新绑定；避免 URL 暴露项目名和 session ID。

## 2. 高权限与认证契约

**可写 tmux 终端按 Operator 级入口保护。** 固定命令不能阻止被授权用户通过 tmux 的其他能力操作该用户环境；因此只向用户本人批准的设备开放，不承诺 workspace 级隔离。不适用于不受信任的访客或共享租户。

网络层将指定设备限制到此 Agent 的专用 HTTPS 端口，检查现有 ACL/grants 的整体效果，不能只加一条窄规则后就称已收紧。若使用身份选择器，说明它会覆盖该用户的哪些设备；要只开放一部手机就使用经核实的设备选择与规则，不能把“同账号设备都能访问”写成“仅这部手机”。[官方规则语义](https://tailscale.com/docs/reference/syntax/grants)是许可并集。

ttyd 只监听 `127.0.0.1`，不监听 `0.0.0.0`、`::`、LAN 或 tailnet 地址。Serve 是唯一允许的网络入口。拟采用 `--auth-header Tailscale-User-Login` 交由 Serve 认证，并在目标版本实测缺 header 被拒、正常 header 通过；**该选项不是用户名白名单**。具体用户/设备授权由 tailnet 策略控制，不能把 header 存在当成授权判断。

[Serve 官方说明](https://tailscale.com/docs/features/tailscale-serve)明确：它会去掉客户端伪造的同名身份 header 再写入可信值；tagged 来源不保证有用户 header；已共享设备的外部访问者也可能有 header。故首版使用经授权、用户身份明确的手机，不分享此设备给访客。缺身份时失败关闭，不为了 tagged 客户端删掉认证检查。

loopback 仍可被 Agent 本机进程访问，其他本地用户也可能伪造 header，**并非 UID 隔离**。P56 必须记录 Agent 的本机信任范围；多用户/不可信本机进程场景不按本方案通过，另行设计 Unix socket 权限或独立认证代理，不能临时把浏览器入口移到 root。风险无法接受时保留方案一、不部署方案二。

禁止把 token/Basic Auth 密码放进 URL、命令参数、日志或仓库；首版不再叠加自建账号系统。开启 HTTPS 可能涉及 tailnet 设置与证书透明度公开设备域名，应在授权清单中说明，见[官方 HTTPS 文档](https://tailscale.com/docs/how-to/set-up-https-certificates)。

## 3. P55：最小适配与离线产物

### P55.1：目标绑定与公共入口

建议实现一个小型 Python 3 helper，放在拟新增 `scripts/mobile/attach.py`；不新增网络框架或 crate。它只读取一个由 Operator 管理的本地 JSON 配置，不依赖 ccnm 私有 session 文件格式。

候选配置字段：`schema_version`、`ccnm_bin`、`ccnm_config`、`expected_operator_uid`、`workspace`、`agent_instance`、`session_id`，以及确有必要时的既有 state 路径。实现时固定 schema，未知字段拒绝；具体路径由部署工具填写，不硬编码 `/Users/bing`、Homebrew 路径、fodelf 或 hpsrv IP。

配置不得存 AI/SSH 凭据；文件仅 Operator 可写，目标为 0600、私有目录 0700，拒绝错误属主、过宽权限、异常符号链接、NUL、空值、参数注入和被替换的目标。脚本、解释器、ccnm 二进制与父目录不可由 Runtime 身份或网络请求修改。现场核对实际 UID，而不是只比用户名字符串。

以参数数组直接 exec 公共 `ccnm attach`，不使用 `shell=True`、`eval`、`sh -c` 拼接。必要环境变量只来自受控配置；复用当前 Agent 配置/状态，不生成第二套 ccnm state 导致无法识别已有会话。日志不打印完整环境或配置内容。

### P55.2：失败与连接语义

只允许绑定既有 ccnm session ID；最终 workspace/instance/session 匹配仍交公共 ccnm 校验。禁止调用 `ccnm run`、官方 CLI、`tmux new`，禁止“找不到则取最新/创建/退到 shell”。过期或错误目标显示简短错误然后退出。不把 CLI 自由文本当稳定 JSON 解析；不新增一套会话状态账本。

每次浏览器连接可以新建 attach 客户端，**不能新建 Agent**。断开/关闭页面只结束该 attach 客户端；信号不得传给整个 ccnm tmux server、Controller 或受管 Agent。默认不自动调用 stop、不改写锁、不无限重试；输入不自动重放。终端内用户主动 `/exit` 或执行退出命令则属于用户行为，可能结束会话，手册必须区别说明。

目标配置使用原子替换。换绑不会自动改变已连终端的目标，操作说明要求先关闭现有 Web attach 客户端，再更新绑定并重连；不得让旧连接和新连接在界面上看似同一任务。

### P55.3：ttyd 参数与进程模板

以目标安装版本的 `ttyd --help` 为准，记录版本与来源。官方[README](https://github.com/tsl0922/ttyd/blob/main/README.md)提供可写、Origin 校验、认证 header、连接数和反代参数；以下为**待验证的参数合同，不是已部署命令**：

| 参数/设置 | 决策 |
| --- | --- |
| `--interface 127.0.0.1`、专用 `--port` | 仅 loopback；端口预检冲突则失败，不占用别人的 listener |
| `--writable` | 明确启用输入；默认只读不能冒充交互验证 |
| `--check-origin` | 必开；在实际 Serve 下验证合法 Host/Origin/端口组合，异常不能靠关检查修复 |
| `--auth-header Tailscale-User-Login` | 配合前述网络授权与可信代理，缺身份拒绝；不等于 header 值白名单 |
| `--max-clients 1` | 首版限制同一 ttyd 的浏览器连接；不是全体系单输入锁，断网后旧连接占位须可恢复 |
| 不启用 `--url-arg` | URL query 不能传 workspace、session、程序或其他 argv |
| 不启用 `--once` / `--exit-no-conn` | listener 不是随一次浏览器断开而结束；退出/重启语义单独验证 |

末尾执行固定解释器、helper 路径与本地配置路径；不以 `bash`、`login`、SSH 跳板或任意用户命令作默认程序。不启用文件传输附加功能或网页录屏。ttyd 的 Origin 检查只提供相应 WebSocket 防护，不当成独立身份认证或完整 CSRF 防护；测试异常 Origin 与伪造身份的实际行为。

拟生成一个独立于 ccnm Controller 的 macOS LaunchAgent plist，使用参数数组与绝对路径，不依赖交互 shell PATH；用户级运行、有限重启/节流、最小日志。预览/生成只写指定输出目录，P55 不 bootstrap、不安装、不调用 Serve。macOS Tailscale CLI 可能需要其 App 实际路径或 `TAILSCALE_BE_CLI=1`，应按[官方 CLI 文档](https://tailscale.com/docs/reference/tailscale-cli)核实，不能把 shell alias 写成 launchd 可执行路径。

### P55.4：离线回归与交付

拟新增 `tests/test_mobile_attach.py`：fake ccnm 记录 argv，覆盖正确 ID、错误配置/权限/属主、过期会话、二进制缺失、注入、无 shell fallback、无 run/new-session，以及重复连接不重复创建 Agent。配置/模板生成器如被新增，测试重复生成、冲突与回退 manifest，不动真实系统资源。

PTY/信号测试使用隔离的 mock 程序或独立测试 tmux socket，不连接真实 Agent、不调用模型。验证 attach 客户端收到断开信号后退出、被监督的测试工作仍在；该证据只属于 helper/模板，不能冒充真实 ccnm + Serve + 手机通过。

P55 交付：小型 helper、配置示例、ttyd/LaunchAgent 模板、自动测试、候选安装/回退清单。文档与脚本门禁通过后停止，P56 才能在授权环境部署。

## 4. P56：私网部署与真机联合验收

### P56.1：基线与授权

复核 P54 环境，先有可用 SSH 救援连接。只读查看 ttyd/Tailscale 版本、Serve/Funnel 现有配置、监听端口、tailnet 许可范围和 Controller 身份；检查 P52 修复版本未回退。不得为了升级 ttyd 一并升级所有 Homebrew 包。

列出本轮安装文件、LaunchAgent label、配置路径、端口、Serve 精确路由、HTTPS/网络策略变更及模型预算。第三方二进制的版本、下载来源、校验信息落入证据，不能只写“latest”。本轮授权不包括重启 Agent、修改全局电源策略、root SSH 或 Runtime egress。

### P56.2：有序部署

先安装受控 helper/配置，启动 loopback ttyd，验证无身份与 Origin 负例；再在网络限制确认生效后开启 Serve。首版用专用 HTTPS 端口的根路径，避免路径改写与同端口其他应用授权混用；端口是现场分配值，不抢已有 443 路由。

候选命令形状来自[官方 Serve CLI](https://tailscale.com/docs/reference/tailscale-cli/serve)，部署时按本机帮助核实：

```text
tailscale serve --bg --https=<本轮专用HTTPS端口> http://127.0.0.1:<ttyd端口>
```

`--bg` 让 Serve 配置在 Tailscale 重启后仍可恢复，不证明 ttyd、Controller、GUI 登录或 AI 会话也恢复。不启用 Funnel、不使用 TCP 裸转发替代 HTTPS。每次变更后核实本端口确为 tailnet-only；Serve/Funnel 共用端口的后续配置可能改变暴露面，不能只看一次安装输出。

不要在服务运行时周期性执行 `ccnm doctor` 作为探活。网页连通、attach 返回、Agent 活跃与 Runtime 可执行分别检查，只有需要并具备空闲窗口时才做 MCP probe。

### P56.3 至 P56.5：验收矩阵

| 编号 | 测试 | 通过条件 |
| --- | --- | --- |
| WEB-01 | 手机蜂窝网络 + Tailscale 打开 HTTPS 页面 | 证书验证正常；绑定的 workspace/instance/session 与 SSH 核对相同 |
| WEB-02 | 中文 IME、多行、Ctrl/Esc/Tab、审批、滚屏/复制、横竖屏 | 关键控制可用；手机无实体键盘时也能批准/拒绝和离开，不复制到未知剪贴板服务 |
| WEB-03 | 锁屏、切后台、刷新、关闭页再打开，各至少三轮 | 只重建 attach；Agent/session/writer 不变；不重放输入；无永久 max-clients 占位 |
| WEB-04 | SSH → Web → SSH 接力，另开第二网页 | 同一会话；第二网页按连接数策略拒绝/等待；不踢桌面，不宣称跨入口输入互斥 |
| WEB-05 | 修改/构建一个可回滚项目任务 | 复用 P54 的 hpsrv 独立核验；窗口展示不是 Runtime 执行证据 |
| WEB-06 | 无 Tailscale、未授权设备、被撤权设备 | 连接被拒；公共网络不能访问；不要只验证首页，应覆盖 WebSocket 握手与终端输入 |
| WEB-07 | 伪造身份 header、缺 header、异常 Origin、URL 注入 | 经代理不能伪装授权；非法 WebSocket 拒绝；参数不会改变执行目标；本地伪造风险按第 2 节披露 |
| WEB-08 | 外部直连 ttyd 端口，包含 LAN/tailnet IPv4/IPv6 | 后端不暴露；没有 wildcard/IPv6 意外监听；无公网端口映射/Funnel |
| WEB-09 | helper 失败、会话已结束、配置换绑 | 错误后退出，不出现 shell、不自动起新 Agent、不 attach 其他会话 |
| WEB-10 | 停 ttyd、重启入口 LaunchAgent、撤 Serve 路由 | 仅入口客户端受影响；原 Agent/MCP/项目任务继续；SSH 救援有效 |
| WEB-11 | 仅断开测试会话 MCP，再显式 stop | 遵守 ccnm 收尾/unknown/交权契约；不能为连通性而解除写锁；覆盖普通命令与已修复 relay |
| WEB-12 | 撤权时保留一个已连接浏览器与一个 SSH 客户端 | 新连接拒绝；现有访问是否仍可输入单独核实并精准终止；Agent 可按普通撤权方案继续运行 |

WEB-02 的移动键盘问题优先用上游客户端选项解决，不能靠取消审批。确需极小的终端按键条时单列改动、无任意命令 API，不发展成聊天平台。未实测的手机/browser/provider 保留未验。WEB-11 的故障注入仅限授权测试目标，日用任务不得被连带杀死。

### P56.6：回退和正式说明

先关本轮 Serve 精确端口/路径，再停止本轮入口 LaunchAgent/ttyd；确认正在工作的 ccnm Agent 与写锁未被误杀。按安装 manifest 撤销本轮资源，不做全局 `serve reset`、kill-server 或整份策略恢复；外部并发修改存在时先核对，不覆盖。

完成后在拟新增 `docs/mobile-access.md` 写手机短手册和救援入口，更新支持矩阵、运维、排错，保存 `docs/research/mobile-web-<日期>.md` 实测记录。只完成内网服务/桌面浏览器验证时不把 P56 标 completed。

## 5. 不混入本阶段的项目预览

查看 hpsrv 的 Web 应用运行效果需要**另一条独立的预览路由**，不是 ttyd 的一部分。本阶段不开放任意端口代理，也不默认暴露项目 dev server；日后按项目逐个授权、记录地址和生命周期。MCP 下启动的开发进程仍绑定会话；持久在线服务归独立服务监督与部署流程，不用 nohup/脱组绕开 ccnm 收尾。

## 6. 实施者接续提示

```text
先完成 P54。P55 只产出 attach-only helper、配置/ttyd/LaunchAgent 模板与离线测试。
不写 Web Agent launcher，不扩充 RPC，不解析任意 URL argv，不给 root 或 ccrun 运行入口。
固定 command 不等于 sandbox：按本人 Operator 终端授权，不面向第三方共享。
P55 完成后停。P56 在具体安装、Serve、HTTPS、网络策略和模型额度获准后才部署。
逐项完成 WEB-01 至 WEB-12，确认手机断线没有新 Agent、撤入口不停止项目任务。
任何需要关 Origin/认证/写锁才能跑通的情况都记为失败，不用降级方案换绿灯。
以现有 Git/status/evidence 交接，不依赖此聊天或某个 MCP 工具才能接续。
```
