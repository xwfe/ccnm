# 跨入口安全与并发（P11 离线部分，2026-09-11）

P11 要回答的是一句话：**第二个入口有没有把第一个入口已经建立的边界撑大。**

这一轮把能离线证明的部分做完了（P11.1、P11.2、P11.4，以及 P11.3 里那个 provider-neutral 客户端）。**真实 Claude Code / Codex 那一半没有做**：它要重建真机环境、消耗订阅额度，需要单独授权，`status.json` 里记着 blocker。

## 一、两个入口抢同一把锁（P11.1）

```text
受管会话（internal 协议 4）──┐
                            ├─ 同一棵工作树，同一把 write guard
外部 coding（协议 5）    ────┘
```

测试 `a_managed_session_and_an_external_one_take_the_same_guard` 两个方向都跑：

- 受管会话先开着 → 外部 coding **启动失败**，`CCNM_E_POLICY`，措辞里有 write guard；
- 外部 coding 先开着 → 受管 open 同样失败，同一条错误。

"失败"是关键：它不是连上之后写不进去，而是**根本没起来**。两个 Agent 同时改一棵树这件事，在第二个进程拿到任何工具之前就被拦住了。

配套的 `a_read_session_coexists_with_a_managed_writer` 证明反面：read 会话不碰锁，所以它既不会拖住受管写者，也不会因为写者在就连不上——它本来就没有能改东西的工具。

**这些用的是同一个 workspace**：fixture 里的 `demo` 既有 `agent = { node, instance }`（受管），又有 `external_mcp`（外部）。一棵树、两个入口，正是要测的形状。

## 二、annotations 是提示，不是门禁（P11.2）

两件事分开证：

**提示是准的。** `a_host_that_ignores_annotations_gains_nothing` 从 coding 会话里读出所有 `readOnlyHint: true` 的工具，和 read 会话实际拿到的工具表比对——除了 `read_output` 完全一致。那一个例外是设计好的：它确实只读，但 read 会话没有能产生 `output_ref` 的工具，给了也是死工具（理由在[契约第 4.3 节](../protocol/remote-workspace-mcp-v1.md)）。

**忽略提示什么也拿不到。** 同一个测试接着扮演一个从不读 annotations 的 Host：在 read 会话里直接按名字调 `exec_command` 和 `apply_patch`，参数都合法（否则被拦下的会是参数检查，那什么都证明不了）。两次都是 `CCNM_E_POLICY`，而且事后断言 `sneaked.txt` 不存在——被拒的写没有偷偷发生。

## 三、一个与 ccnm 无关的客户端重放同一套矩阵（P11.3 的一半）

`tests/mcp_client.py` 不 import 任何 ccnm 代码、不装任何第三方包，只知道"MCP 是一行一条 JSON-RPC"；连 internal 协议 5 的 payload 都是 `tests/test_remote_workspace_mcp.py` 自己用 `json` + `base64` 拼的。

这不是把 Rust 那边的测试再写一遍。那边的客户端和服务端同仓库、同语言、共用同一批类型；**两个独立实现得出同一个结论，结论才属于协议而不属于某段代码**。10 条用例覆盖：read 正好四个工具、只读工具真能用、三个被收起来的工具用合法参数调仍被拒、coding 七个、annotations 逐项对、没 opt-in 与不存在同样拒绝、越权拒绝、未知协议号停下、stdout 每一行都是 JSON-RPC、握手里不出现本机路径。

它也不假装是任何 provider：`clientInfo.name` 填的是 `provider-neutral-test-client`。服务端不该从那个字段推断任何东西，而它确实没有。

## 四、跨入口回归（P11.4）

| 关心的事 | 证据 |
| --- | --- |
| 凭据泄漏 | Runtime 进程里放一个像 Agent 凭据的环境变量，外部会话**也**起不来（`CCNM_E_POLICY`，正文说的是检查名，不是值）。这条边界不属于受管入口 |
| private path | 外部会话读 `../config.toml` 被拒，错误里不出现本机任何绝对路径 |
| SSH agent forwarding / 连接复用 | bridge 造的命令带 `ClearAllForwardings=yes`、`ControlMaster=no`、`ControlPath=none`、`BatchMode=yes`、`SendEnv=-*`，且任何地方都没有 `ForwardAgent=yes`。bridge 跑在客户端机器上，那里通常真有 ssh agent 和用户自己的 ssh 配置，一样都不能跟着进 Runtime |
| 断连残留 | 会话被 kill：客户端读到 EOF；**写权限不自动转让**（锁文件里留着 `held`，下一个 coding 被拒），read 照常开 |
| 输出保留 | 一个会话的 `output_ref` 在另一个会话里解析不出来——它是会话内的引用，不是机器上的句柄 |
| busy / unknown | 上面第一节（busy）与断连残留（unknown）各有用例 |

## 五、验证

```text
cargo fmt --all --check                                通过
cargo clippy --workspace --all-targets -- -D warnings  通过
cargo test --workspace                                 674 passed / 0 failed（本轮净 +6）
python3 -m unittest discover -s tests -q               156 passed（本轮净 +10）
python3 scripts/check_protocol.py                      通过（38 + 21）
python3 scripts/check_plan.py                          通过
git diff --check                                       通过
```

## 六、没有做的那一半

**P11.3 的真实 Host 部分没有做，也不能靠这些离线证据代替。** 缺的是：

- 用真实 Claude Code 的标准 MCP 配置（`mcpServers` 里那几行）跑一遍七工具/只读允许矩阵；
- 同一套东西在 Codex 或另一个真实 Host 上再跑一次；
- 看它们怎么展示 bridge 的 stderr——`CCNM_E_*` 那行到底有没有到人眼前；
- 真 ssh：选项被真实 sshd 怎么处理、断线重传、连接建立失败的真实措辞。

这些都要重建真机环境（Runtime 账号、SSH 准入、两端部署）并消耗订阅额度。P7 的环境已按用户要求归零，脚本可重跑，但**属于需要单独授权的动作**，所以这一轮停在这里。

在那之前，支持矩阵里 Remote Workspace MCP 这一项仍然是 **experimental**：这一轮把"它没有撑大边界"证到了离线能证的尽头，没有把"它在真实 Host 上能用"证出任何一点。
