# Provider 安全契约（P1）

本契约约束 Claude/Codex 的既有远程执行链路，不新增公开配置、Provider、RPC 或生产权限。Provider 声明已测需求，`safety`、SSH、Controller/session 和 Runtime 执行它们。native colocated 是受信任本地执行，不是隔离 Runtime；其已知支持缺口留给 P3。

## 身份、状态和来源

| 对象 | 权威来源 | 可以做什么 | 不可以做什么 |
| --- | --- | --- | --- |
| Agent 私有目录与认证 | Agent 的本地配置、OS 执行身份、官方 CLI | 检查目录/file metadata；官方 CLI 独立登录与报告状态 | 读取认证内容、为新 profile 接受 Runtime 下发的私有路径、复制/代理订阅凭据 |
| Agent → SSH 环境 | Agent 进程 | 清除所有已知 Provider 的私有环境和其他凭据形态的变量；本机 SSH 可使用自己的认证 agent | 转发认证 agent、继承未知安全策略的 ControlMaster、用配置字面值重新注入认证环境 |
| Runtime 项目环境 | Runtime 执行身份的启动环境与用户明确提交的项目命令 | 保留普通工具链/项目变量；项目命令的显式 env 参数仍属于已授权任意 exec | 把来源不明的 ambient 认证变量当项目变量；宣称 parser 能约束任意 shell |
| Runtime 可访问凭据 | 当前执行身份的 HOME、Provider 声明的默认/额外目录及 Runtime-local 环境引用 | 对所有已知 Provider 做有界路径、环境与可访问性检查 | 只查本次选中的 Provider；枚举其他用户目录；读取秘密或把路径/值放进报告 |
| 网络要求 | Provider 的已测 endpoint 元数据 + 用户声明的 OS/network 策略 | 单独诊断可达性，明确范围 | 把域名列表当防火墙、把一次不通当完整 egress 隔离证明 |

普通变量不是“任意字符串都安全”的承诺。P1 没有新增项目秘密授权 API：已知 Agent 前缀（包括 Codex 已测的整个 CODEX/OPENAI 前缀）继续移除；未知的 token/password/secret/credential 等名称形态按未授权认证处理。`GOOGLE_CLOUD_PROJECT` 等普通项目标识不因供应商品牌被一刀切删除。显式命令可自行构造环境，这是任意 exec 的既有权限，不是凭据隔离绕过的防御目标。

兼容例外必须明确：Claude v1 的 `claude_config_dir` 仍保留原有调用方参数语义，本阶段不擅改用户配置，不新增私有路径发现或回传。它不是新 profile 的授权模型；P2 负责 Agent-local 引用与 legacy 冲突/迁移契约。Codex 专用 HOME 始终只由 Agent 自己解析。

Runtime 启动环境（包括 SSH 服务端 `AcceptEnv`）必须由 Runtime 运维明确控制。P1 不新增环境授权配置，也不证明任意 SSH 配置不会传递普通变量。名称分类不是扫描秘密值：藏在任意名称、任意文件或项目输出中的秘密不在这份诊断的证明范围。

离线实测表明 `SendEnv=-*` 不一定清除后续配置条目，因此不能替代进程环境清理。所有 SSH 命令另外设置无秘密的 `SetEnv=CCNM_TRANSPORT=1`，覆盖可能注入字面值的配置列表；`SetEnv=none` 实际无效。清理不删除 Agent 本机 SSH 认证所需的 `SSH_AUTH_SOCK`，但禁止 forwarding，且不复用旧连接。MCP 最终仍固定 `/usr/bin/ssh`，不改用 PATH 中的同名程序。

## 检查语义

- `missing`：对已知路径未发现文件；仅说明这一个候选路径。
- `inaccessible`：当前身份的 OS 访问检查明确拒绝；不以“属主不是我”或 Unix mode bits 猜 ACL 的结果。
- `accessible`：可以读取已知认证文件/容器，失败。不实际读取内容。
- `unknown`：身份、目录引用、metadata、symlink、访问探测、权限错误或超时不能解释，失败。未知不能输出“机器没有秘密”。

只检查两 Provider 的已知文件位置，包括默认 Codex 目录和 ccnm 专用目录；额外/自定义位置只取 Runtime 本地来源。macOS login Keychain 容器的可访问性是保守风险信号，不读取 item，不声称它一定有 Claude token，也不能据此证明 Keychain 服务级 ACL。任意其他路径、凭据代理或 OS credential service 的完整隔离需 P3 专用身份/ACL 实测。

Agent 的专用 Codex HOME 必须是当前有效 UID 所有、仅属主可访问、无符号链接的目录；不能把 HOME 目录的属主当执行 UID。认证文件同样要求私有、正规文件且非 symlink。官方 CLI 探测是受控诊断，政策失败不能拉起项目命令或 Agent 会话。

Runtime 初始化先检查身份和凭据边界，失败时不进入 Git 等依赖项目的子进程；exec 前再复查凭据可访问性和环境，不能仅依赖 session 启动时的旧审计。即使 `allow_unconfined_exec=true`，已知 Agent 凭据可访问/未知、来源不明认证环境仍拒绝；这个选项只能接受既有账号 confinement 风险，不能授权共享 Agent 登录。OS 权限仍须阻止进程在检查后获得新权限：审计不是无竞态 sandbox。

## 分层验收与兼容

1. Agent transport 和预检 SSH：分别检查构造命令、共用 spawn/exec 环境应用函数实际交给测试子进程的合成环境及 OpenSSH 解析选项。MCP launch plan 指向 Agent-local Rust wrapper；测试仅替换 SSH 可执行程序，不连接远端。不借 Runtime 的 `env -i` 证明上游已清理。
2. Runtime child：普通合成项目变量保留；已知 Provider 私有变量不进入 child；未知认证环境或可访问/未知认证路径在 spawn 前失败，以 marker 文件未产生证明。
3. 私有目录与身份：合成 fixture 覆盖错误 UID、权限、symlink、缺失、访问拒绝和未知。测试不读取真实用户认证文件、不安装 CLI、不调用真实模型。
4. Claude CLI 参数、策略和协议保持 golden 原件；SSH 安全选项或 exec 拒绝边界的收紧必须作为明确行为变更单独断言，不能重录原 golden 将其伪装成等价重构。Codex 固定 CLI 策略不放宽。

本阶段不执行建账号、sudo/ACL/firewall 改动或部署。生产安全、真实多身份与网络断连验证留给 P3，离线通过不得提升为生产通过。
