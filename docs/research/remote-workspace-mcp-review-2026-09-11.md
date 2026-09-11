# Remote Workspace MCP 契约评审（2026-09-11）

对 [契约](../protocol/remote-workspace-mcp-v1.md)、[schema](../protocol/schema/remote-workspace-mcp-v1.schema.json) 和 21 个 fixture 的评审记录。

**评审形式先说清楚：单人自审，没有第二个人复核，没有任何真实 MCP Host 连过它，一行实现代码都还没写。** 做法是逐条对照 ROADMAP 的 P9.1…P9.4、对照 [双执行入口方案](../plan/runtime-surfaces.md) 第 7 节的设计约束、再把每一条"实测"的说法拿去仓库里的真实代码和 SDK 源码核对。所以本文的结论是"契约自洽、并且和当前 Managed 路径的实现对得上"，不是"这个入口能用"。

## 一、逐条核对

| 验收 | 落点 | 状态 |
| --- | --- | --- |
| P9.1 CLI/配置形状定稿；一次进程一个 workspace/Runtime；root 在 Runtime 权威解析；禁止把 host/root/private key 当 MCP tool 参数；命令名定稿 | 契约第 3 节（命令 `ccnm mcp bridge`、参数表、禁止项）、第 4.1 节（Runtime 权威 opt-in） | 覆盖 |
| P9.2 external MCP opt-in 与 disabled/read/coding 最大权限；read 没有 apply_patch/exec_command；coding 才竞争 writer guard；调用方不能越权；多租户不在首版 | 第 4 节全节；`start-refused-mode-escalation`、`start-refused-busy`、`tools-list-read` | 覆盖 |
| P9.3 七工具的 ToolSemantics/annotations 映射；annotations 只用于 UX；exec_command 永远保守 | 第 5 节；`tools-list-coding` | 覆盖 |
| P9.4 连接/EOF/interrupt、busy/unknown、输出预算、版本不匹配、instructions/context policy、错误泄漏边界；契约与 fixture 先行，不做真机付费调用 | 第 6–11 节；21 个 fixture；本文 | 覆盖 |

### 与 MCP 规范和 SDK 的核对

凡是写"实测"的地方，依据都是仓库当前依赖的 rmcp 3.2.0 源码，不是记忆：

| 说法 | 依据 |
| --- | --- |
| 服务端默认声明 `2025-11-25` | `ServerInfo::new` 用 `ProtocolVersion::default()`，而 `LATEST = V_2025_11_25`（`rmcp/src/model.rs`） |
| SDK 认识的版本共 5 个（到 `2026-07-28`） | `ProtocolVersion::KNOWN_VERSIONS` |
| 版本"不匹配"实际是协商，不是报错 | `negotiate_protocol_version`：客户端要的是服务端支持的旧版本就照回，否则回服务端最新的旧版本；只有服务端一个带 `initialize` 的版本都没有才回 `-32022` |
| 未知工具名回 `-32602` + `tool not found` | `handler/server/router/tool.rs` 两处 `invalid_params("tool not found", None)` |
| 失败的工作是 `isError` 结果，不是协议错误 | `mcp/server.rs` 顶部的第三条规矩，以及所有工具的实现 |
| 结果只发 text，不发 structuredContent | `text_only()` 的注释：实测 Claude Code 2.1.260 在两者都有时只显示结构化内容 |

### 与现有实现的核对

契约里的每一条都落在 Managed 路径已经有的东西上，没有为外部入口发明第二套：

| 契约 | 实现依据 |
| --- | --- |
| 七工具的名字、描述、参数 | `mcp/server.rs` 的 `#[tool]` 声明 |
| 写入互斥的键是工作树规范化根路径 | `mcp/write_guard.rs`：锁文件名是 resource root 的 FNV-1a |
| busy / unknown 两种拒绝措辞 | 同上，`TryLockError::WouldBlock` 与其他错误分支 |
| coding 在"打开时"就拿锁 | `Server::new` 无条件 `WriteGuard::acquire`，在任何工具调用之前 |
| 输出预算的每个数字 | `read.rs` 2000 行、`list.rs` 1000 条、`search.rs` 200/10/32 KiB/512 B、`exec.rs` 600 s/16 KiB/64 MiB/100 次、`output.rs` 32 KiB、`patch.rs` 50 文件/1 MiB/16 MiB |
| `instructions` 16 KiB | `provider/context.rs` 的 `MAX_INSTRUCTIONS_BYTES` |
| `output_ref` 是 session 作用域 | `exec.rs::session_dir` 的注释与 `output.rs::read_output` 的 join |
| 九个 `CCNM_E_*` 名字 | `error.rs` 的 `ErrorCode` |
| 内部 open 协议是整数且 fail-closed | `runtime.rs` 的 `OpenPayload.protocol`（当前 4） |

## 二、评审当场改掉的问题

这些是评审真正的产出：写的时候按设计文档写下来，拿去对代码才发现不对。

### 1. `read` 模式原本有 5 个工具，其中一个永远不可能成功

ROADMAP P9.2 的下限是"read 模式没有 `apply_patch` 和 `exec_command`"，照着写就是 5 个工具，含 `read_output`。对完实现才发现：`output_ref` 只在**产生它的那个 session 的保留目录**里有意义（`read_output` 就是拿 ref 去 join 本 session 的目录）。read 模式没有 `exec_command`，永远产不出 ref，所以那个工具挂在那里只能必然失败；而让它去解析别的 session 的 ref 就是跨会话泄漏。

改：read 模式定为 4 个工具，并在契约里写明这比 ROADMAP 的下限更窄以及为什么。

### 2. "版本不匹配会报错"是错的

初稿照着直觉写"MCP 协议版本不匹配 → 报错"。去读 SDK 的协商函数才发现实际行为相反：服务端会安静地协商到一个双方都有的版本，`-32022` 只在服务端连一个带 `initialize` 握手的版本都不支持时才发——ccnm 永远走不到那一步。

改：第 9 节按实际行为重写，并明确区分"MCP 版本会协商"和"ccnm 内部 open 协议不匹配就停"。这两件事写在一起过一次，读的人一定会混。

### 3. 启动失败没有可检查的表达

workspace 没开放、越权、guard busy、远端版本太旧——这些全发生在 `initialize` 之前，所以根本没有 MCP 消息可以写成 fixture。初稿就这么留着，结果是**契约里最容易出错的一半没有任何机器可检查的样例**。

改：把"启动失败"定义成一个稳定的可观测结果——退出码非 0，stderr 最后一行 `CCNM_E_*: 一句话`——给它一个 schema 定义和 9 个 fixture。契约里同时写明这不是协议消息，不保证结构化解析。

### 4. 越权请求原本打算降级

配置 `read`、请求 `coding`，初稿写的是降级成 read 让它先连上。想了一遍 Host 那边会发生什么：模型一路调 `apply_patch`、一路收到"没有这个工具"，最后大概率报告"我已经改好了"。

改：拒绝启动，理由写进文档。宁可给一条人能看见的错误，也不给模型一个它读不懂的环境。

### 5. read 模式的工具白名单只是一句话

初稿里"read 模式只有这四个"只写在表格里，schema 对 `tools/list` 一视同仁。这种约定第一次改代码就会破。

改：schema 加 `read_tool_name` 白名单和 `tools_list_result_read`，read 的工具表用后者；往里塞一个 `exec_command` 现在会被检查脚本挡下来（`test_read_mode_cannot_advertise_a_write_tool` 就是这条）。

### 6. `anthropic/requiresUserInteraction` 差点被照抄过来

Managed 路径的交互式 session 会给 `exec_command` 带这个 `_meta` 键，因为那时确实有人坐在终端前。外部 MCP 不知道 Host 那头有没有人。

改：明确不发这个键。冒充知道比不说更糟——Host 可能因此跳过它本来会做的审批。

### 7. 检查脚本只认一套协议

`check_protocol.py` 原本把 schema/fixtures/spec 三个路径写死，错误检查也只认 JSON-RPC 数字码。这套契约的错误是 `CCNM_E_*` 名字，硬塞进去会把两种错误模型搅在一起。

改：脚本改成两个 bundle，各自带自己的错误检查方式；两边的规矩一样——文档里写了的必须有 fixture，fixture 里出现的必须写在文档里。

## 三、明知而留下的

1. **命令名 `ccnm mcp bridge` 没有被任何真实 Host 配置验证过。** 它在 Claude Code 的 `mcpServers` 里怎么写是照规范推的，P11 真连的时候可能发现参数形状要调。
2. **断线不自动重连。** Host 那边看到的就是"这个 server 没了"，要人重开。自动重连意味着换一个远端 session，而调用方手里的 `output_ref` 属于旧的——宁可让它明确地死。
3. **`external_instructions = "project"` 用固定顺序 `AGENTS.md` → `CLAUDE.md`。** 对一个以 `CLAUDE.md` 为主、同时又有 `AGENTS.md` 的项目，外部客户端拿到的可能不是它期望的那份。但猜调用方是谁更糟，而且顺序是 Runtime 侧配置的人能预期的。
4. **read 模式的并发连接数没有上限。** 每个连接都是一条 SSH 加一个远端进程；真正的限制来自 sshd 和机器本身，契约不假装管这件事。
5. **busy 在这条路上只能表现为"server 起不来"。** Machine API 那边有 `-32008` 这样的码可以让调用方退避重试，MCP 这边没有对应表达——Host 的 UX 因此不好看。真要改善，得等真实 Host 证明这个体验确实卡人。
6. **没有 `tools/list` 变更通知。** 因为模式在一条连接的生命周期里不变。将来若支持中途升权，这条必须补。
7. **`ccnm mcp bridge` 与现有 `ccnm mcp probe` 共处同一个子命令组。** 两个名字都以 mcp 开头但角色不同（一个是长活的桥，一个是一次性诊断），P10 实现时要确保帮助文本把这件事说清楚。

## 四、这份契约还没有被什么验证过

- **没有实现。** 文档里定稿的命令、配置字段和行为在当前二进制里一个都不存在，`ccnm mcp bridge` 打上去只会报"未知子命令"。
- **没有真实 MCP Host 连过**（P11），**没有远端真实项目 dogfood**（P12），**没有非 macOS 证据**。
- **没有第二个人复核。** 上一份契约（Machine Protocol v1）也是单人自审，它在 P6 的黑盒消费者和 P7.3 的真机里被改过——这份大概率也会。
- 契约里对 Host 行为的假设（会展示 stderr、会尊重或忽略 annotations）都没有实测，只写了"忽略也必须安全"这条对 Runtime 的要求。

## 五、本轮验证

```text
python3 scripts/check_protocol.py                              通过（38 + 21 个 fixture）
python3 -m unittest tests.test_check_protocol -q               24 passed（本轮新增 6 条）
python3 scripts/check_plan.py                                  通过
git diff --check                                               通过
```

没有跑 Rust 门禁：本轮没有改任何 Rust 代码。没有启动 Agent、没有拨 ssh、没有消耗任何订阅额度。
