# Remote Workspace MCP bridge 实施记录（P10，2026-09-11）

把 [P9 的契约](../protocol/remote-workspace-mcp-v1.md)做成能跑的东西。**没有真实 MCP Host 参与，没有拨过一次真 ssh**：全部证据来自真实二进制 + 真实 MCP 消息 + 管道，以及一个假 ssh。真实 Host 的允许矩阵是 P11，远端真实项目是 P12。

## 一、做了什么

| 位置 | 内容 |
| --- | --- |
| `crates/ccnm-core/src/mcp/bridge.rs` | `ccnm mcp bridge` 的本机半边：挑 node、编 payload、造出那一条 ssh 命令 |
| `crates/ccnm-core/src/runtime.rs` | internal 协议 5：`ExternalOpenPayload` 与 `open_external`，opt-in 与模式判定 |
| `crates/ccnm-core/src/mcp/server.rs` | 按 entry 决定要不要抢写锁、发几个工具、调用时拒绝哪几个、发什么 instructions、发 annotations |
| `crates/ccnm-core/src/config.rs`、`instance.rs` | `external_mcp`、`external_instructions` 两个字段，以及"没有 Agent 的 workspace 何时合法" |
| `crates/ccnm-core/src/provider/context.rs` | 外部客户端的 instructions：generic / project / none |
| `crates/ccnm-cli/src/main.rs` | `mcp bridge` 子命令与 `internal mcp-serve` 的第三种分派 |

### 为什么 bridge 不 fork

它做完本机检查就 `exec` 成那条 ssh。于是**这个进程就是 transport**：EOF 和信号直接落在 ssh 上，没有第二个进程可以变成孤儿，也没有"回收子进程"的代码需要写对。契约里原本写着"bridge 必须回收自己的 SSH 子进程"，实现之后改成了现在这句——承诺没变（不留孤儿 transport），达成方式比原来简单。

同一个理由让 bridge 不做自动重连：重连意味着换一个远端 session，而调用方手里的 `output_ref` 属于旧的。

### 为什么是第三个协议号

协议 4（Managed）说的是"这个 Agent Node 的这个 instance，由 ccnm 启动"；协议 5 说的是"一个这里不管理的 MCP 客户端，provider 未知且不许猜"。信任模型不同就给新号——和当初 4 从 3 分出来的理由一样。旧 build 收到 5 会停在 `CCNM_E_VERSION`，而不是当成 Managed 打开然后发出七个工具。

`deny_unknown_fields` 让"不能夹带 root"成为 wire 属性：塞一个 `root` 进去是解码错误，不是被忽略的字段。

### 两层门禁，不是一层

`tools/list` 不列某个工具是**提示**，Host 可以不理。所以每个被收起来的工具在处理函数第一行还有一次拒绝：read 模式下调 `apply_patch` 拿到的是 `CCNM_E_POLICY`，不是"工具不存在"。测试 `a_read_session_offers_four_tools_and_refuses_the_rest` 就是照着这条写的——它用**合法参数**去调，否则参数检查会先失败，那样什么都证明不了。

## 二、实现推翻的三处契约

### 1. 启动诊断不是一行

契约原本写"stderr 最后一行形如 `CCNM_E_POLICY: ...`"。实际上 ccnm 只有一个错误打印路径，形状是：

```text
CCNM_E_POLICY:
workspace myproject allows external MCP in read mode; coding was requested
```

而且远端拒绝和本机拒绝长得一模一样——打印它们的是同一段代码，远端那份由 ssh 原样带回来。**为了契约好看而在 bridge 里造第二种错误格式是错的**，所以改的是契约：第一行是名字，后面是解释；要机器判断就看退出码和第一行。schema 与 9 个 fixture 同步改了。

### 2. annotations 原本只写在纸上

契约第 5 节列了七工具的 `readOnlyHint` / `destructiveHint` / `openWorldHint`，而 P9 结束时实现一个都没发。这一轮补上：五个只读工具 `readOnlyHint: true`、`openWorldHint: false`；`apply_patch` destructive 但不是 open-world；`exec_command` 两者都是，**不看这次的命令长什么样**。

它们仍然只是 Host 的审批 UX——忽略它们的 Host 得到的授权结果完全一样，那由上一节那两层门禁保证。

### 3. 被杀掉的会话不把写权交给下一个

原本顺手写了个测试，假设"进程没了锁就自由了"。实际不是：flock 确实随进程释放，但锁文件里留着 `held`，下一个 coding 会话因此被拒——`CCNM_E_POLICY: ... left held by an interrupted process`。

这不是 bug，是 P3 定下的规矩：证明不了旧执行者的子进程都结束时，宁可停在 unknown 等人。测试改成断言这条行为，并且顺带证明 read 会话照常打开（它本来就不要锁）。

## 三、配置上的一个新事实

**只给外部 MCP 用的 workspace 可以没有 Agent。**

原来的校验要求每个 workspace 必须有 `agent` 或 `agent_node`。对一个放在 NAS 或服务器上、只由本机 Claude Code 通过 bridge 访问的项目来说，那会逼人写一个不存在的 Agent Node 进配置——配置从此开始说谎。

现在的规则：没有 Agent 也没有 `agent_node` 时，只要 `external_mcp` 不是 `disabled` 就合法，并且必须定义在自己的 Runtime Node 上（root 的权威只有一份）。Managed 路径打开这种 workspace 仍然会失败，因为它绑不出 binding——这正是想要的。

## 四、验证

```text
cargo fmt --all --check                                通过
cargo clippy --workspace --all-targets -- -D warnings  通过
cargo test --workspace                                 668 passed / 0 failed（本轮净 +30）
python3 scripts/check_protocol.py                      通过（38 + 21 个 fixture）
python3 -m unittest discover -s tests -q               146 passed
python3 scripts/check_plan.py                          通过
git diff --check                                       通过
```

新增的 30 条里，16 条是 `crates/ccnm-cli/tests/external_mcp.rs` 的集成测试，跑的是真实二进制、真实 MCP 消息：

- read 模式正好四个工具；三个被收起来的工具用合法参数调也被拒；只读工具照常能用；
- coding 模式七个工具，`apply_patch` 真的写进了文件；
- coding 抢写锁（第二个 coding 启动失败，措辞里有 write guard），read 不抢（能和它并存）；被 kill 之后不自动接管；
- 没 opt-in 的 workspace 和不存在的 workspace 同样拒绝，措辞只差调用方自己送来的名字；
- 越权请求拒绝启动，stdout 一个字节都没有；
- generic 不投影项目文件，project 按 `AGENTS.md` → `CLAUDE.md` 的固定顺序投影；
- 外部工具表不带 `requiresUserInteraction`；annotations 逐项正确；
- 路径策略与 Managed 路径一致，错误里不出现本机绝对路径；
- **假 ssh 跑通整条链**：bridge 造出来的 argv 经"ssh"进到真实 server，握手、列工具、读文件都成立；
- 远端不可达（假 ssh 返回 255）不产生半个握手；远端是旧 ccnm 时，它自己的 `CCNM_E_VERSION` 原样传回；
- 乱七八糟的输入（非 JSON、缺 method、1 MiB 的一行）不破坏流，下一个正常请求照常回答；
- 服务端被杀是 EOF，不是静默成功。

## 五、没有验证的

- **没有真实 MCP Host。** Claude Code 和 Codex 的 MCP 配置形状、它们怎么展示 stderr、它们尊不尊重 annotations，全部未测——那是 P11。
- **没有真 ssh，也没有远端真实项目。** 假 ssh 只证明 argv 可用，不证明选项、认证、断线重传的行为；远端 Linux 更是完全空白（P12）。
- **跨入口并发只测了外部之间。** "Managed coding session 与外部 coding 抢同一把锁"按 ROADMAP 属于 P11.1，这里没做。
- 没有真机、没有付费调用、没有部署，也没有碰任何系统配置。
