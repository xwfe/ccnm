# P69 doctor 先报版本、命令审批按 Provider 说、workspace add 认配置里的节点名（2026-10-04）

接 [P62 续跑记录](2026-10-04-p62-resume-release.md)第 7 节的 F20、F21、F24。同一天另一个会话在做 P68（F22、F23，[记录](2026-10-04-p68-supervisor-gone-lost-output.md)）；这三条按用户的分工由本会话做，代码先在分支 `claude/musing-antonelli-565039` 上写，P68 完成后再立 P69、补计划与状态、合进 main，所以这个分支上计划提交排在代码之后。

**只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0；没有跑模型、没有推送、没有部署，三条都没有在真机上复验。（后注：同日晚 F20、F24 在 P62 的真机上零额度复看通过，F21 没看，见 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)第 6 节。）`ccnm.machine/1` 的线格式、内部协议号、版本号都没变。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P69.1 F20 | Agent 报回来的身份对不上时，doctor 先看版本行：版本号或构建不同，就以 `Agent ccnm` 那一行（`CCNM_E_VERSION`）为失败，`Agent selection` 记成没比较。同一个构建答了别的实例，仍是原来那句 `identity differs` | `e9ef28b` |
| P69.2 F21 | `Command approval` 一行按探测报告里的 Provider 写：Codex 是 WARN，写明不问、为什么、什么还在约束命令；Claude 的两种写法不变 | `8cf4d28` |
| P69.3 F24 | `workspace add` 的 `runtime_node` 取 `this`；Agent 节点有叫 `agent` 的用它，否则用 `this` 以外唯一的那个，好几个时要 `--agent-node`（新加），否则报错、什么都不写 | `780e5b9` |
| P69.4 门禁与文档 | 见第 5 节 | 本记录同一提交 |

三条各修在哪一端：F20、F21 是 doctor 自己的判定和措辞，**跑 doctor 的那台**换成新构建就生效，对面是旧版本也一样；F24 是 Runtime Node 上的命令行。

## 2. F20：先报版本，不再只说身份不符

**缺陷**：Operator 侧 doctor 对 instance workspace 收到 Agent 的探测报告后，先比报告里的 Agent 身份和这次选的实例，对不上就写一行 `Agent selection FAIL CCNM_E_VERSION: Agent probe identity differs from the Runtime selection` 并返回。版本行是后面 `probe_rows` 的第一行，根本轮不到。真机上 0.10.1 的 Operator 对 fodelf 上 0.9.0 的 Agent，旧 Agent 没按新构建的意思读请求，报告里没有身份，于是唯一的失败行在说一个症状，原因（两台 ccnm 不一样）没出现。

**改法**（`doctor::workspace_checks`）：身份对不上时先算版本行（与 `probe_rows` 用的是同一个 `version_row`：版本号不同，或版本号相同但内部协议最高号不同/没报）。版本行不是 OK，就写 `Agent SSH` OK、这一行版本失败、`Agent selection` SKIP（`not compared: the Agent Node runs another ccnm build, which may not have read the selection`），然后返回。版本行是 OK 的，走原来的身份失败。

**为什么不渲染其余各行**：原来身份不符时也是直接返回、不渲染；对面是另一个构建时，它报回来的各项是按它自己的理解填的（P66 的 F10 就见过"没探"被当成"同一台机器"），渲染出来反而多出几行可能不实的结论。原因只有一个，表里就只说这一个。

**回归**：`an_agent_on_another_version_is_named_before_any_identity_is_compared`（`doctor::tests`）。探测报告取一份正常报告，把 `hello.ccnm_version` 改成 `0.9.0`、去掉 `wire`（0.9.0 没有这个字段）、不带身份——就是真机上旧 Agent 回来的样子；第二段只去掉 `wire`、版本号相同（同号不同构建）。

| 构建 | 结果 |
| --- | --- |
| 修之前（`f5df624` 上加用例） | 失败：`no row Agent ccnm`，表里只有 `Agent selection FAIL CCNM_E_VERSION: Agent probe identity differs from the Runtime selection`，与真机一致 |
| 修之后 | 通过 |

另加 `the_same_build_answering_for_another_instance_is_still_refused`，守住身份比较本来要拦的那种情况（同一个构建、协议 3、答的是 `codex-main`），它在修之前和之后都通过。写它时第一版忘了把报告的协议设成 3，被 `remote ccnm speaks protocol 1, this message requires 3` 挡在更前面——那是测试写错，不是产品行为。

## 3. F21：Codex 会话不问，doctor 照实说

**缺陷**：`Command approval` 一行只看 workspace 的 `allow_unattended_exec`，不看 Provider，对 Codex workspace 也说 `OK interactive sessions ask before each exec_command, in every permission mode`。真机上受管 Codex 会话里模型调 `exec_command` 直接执行，没有任何审批。

**原因**："每次都问"靠的是 `anthropic/requiresUserInteraction`，只有 Claude Code 认（`mcp/server.rs` 的 `INTERACTION_TOOLS` 一段）；Codex 一侧 ccnm 必须给自己的 MCP 工具设 `default_tools_approval_mode="approve"`，不设的话 `approval_policy="never"` 下每次调用都被 Codex 拒掉（[Codex 探针记录](codex-provider-probe-2026-09-07.md)）。所以 `allow_unattended_exec` 对 Codex 不起作用。文档在 P62 续跑当轮已改（`7f5c054`），这一行是代码。

**改法**（`doctor::runtime_safety_rows` 多收一个 Provider，调用处传探测报告的 `rep.provider`）：Codex 时是

```text
Command approval        WARN   Codex sessions run every exec_command without asking: the per-call prompt is a key only Claude Code reads, and Codex refuses ccnm's tools unless they are approved up front
                               the runtime account's own permissions and the workspace root are what bound them
```

同时设了 `allow_unattended_exec` 的，再加一行 `allow_unattended_exec is set as well; for Codex it changes nothing`。Claude 的 OK（会问）和 WARN（设了不问）两种写法一字没动。

**为什么是 WARN 不是 OK**：这一行回答"会不会停下来问我"。Claude 设了 `allow_unattended_exec` 时同样是"没人问"，那里是 WARN，配置说明写的是"永远不会是 OK"；同一个事实不该因为 Provider 不同就换成绿色。WARN 不阻塞 doctor 的结论。

**不在这一阶段**：要不要给 Codex 补一道审批（续跑记录 F21 的"另行决定"）。（后注：用户 2026-10-07 定补，P71 实现，这一行对 Codex 改成 OK 并说明会话里可以切到 Full Access，见 [P71 记录](2026-10-07-p71-codex-asks-before-exec.md)。）那要改 Codex 的工具配置或 ccnm 自己在服务端拦，都会改变受管 Codex 会话的行为，不是 doctor 的措辞问题。

**回归**：`the_approval_row_says_codex_sessions_do_not_ask`（`doctor::tests`），在 Agent 侧 doctor 上用 Codex 的探测报告渲染整张表。

| 构建 | 结果 |
| --- | --- |
| 修之前（`e9ef28b` 上加用例） | 失败：`Command approval OK interactive sessions ask before each exec_command, in every permission mode` |
| 修之后 | 通过；同一用例里 Claude 那张表这一行仍是 OK、仍说 `ask before each exec_command` |

写它时第一版还断言了整张表退出码为 0，实际是 3——那是夹具本来就有的几行 SKIP（`Native tool policy` 等），与这一行无关，删掉了那句断言。

## 4. F24：workspace add 认配置里的节点名

**缺陷**：`ccnm workspace add` 总写 `agent_node = "agent"`，`runtime_node` 留默认的 `runtime`。真机上 Runtime 的配置节点叫 hpsrv、fodelf、xdwmbp，写出的条目两个节点都不存在，校验不过，被拒、什么都没写。

**改法**：

- `runtime_node`：`workspace add` 在项目所在的机器上跑，Runtime 就是配置里的 `this`。`this` 不叫 `runtime` 时显式写 `runtime_node = "<this>"`（`configedit::Edit::set_workspace_runtime_node`，只在新增或修改时记一条变更）；就叫 `runtime` 或没有 `this` 时不写，和以前一样。
- Agent 节点：`--agent-node <node>` 给了就用它；否则有叫 `agent` 的节点用它（`ccnm init` 生成的配置走这条，写出来和以前逐字相同）；否则 `this` 以外只有一个节点就用那一个；一个都没有报 `CCNM_E_CONFIG` 并提示先 `ccnm init --agent <alias>`；不止一个报 `CCNM_E_INVALID_ARGS`，列出候选、要求 `--agent-node`，**什么都不写**。
- 配置读不出来（还没有、或本身不合法）时照旧用 `agent`、不写 `runtime_node`，由保存那一步报原来的错（"还没有配置，先 init"之类）。
- `--agent-node` 带中文帮助（`zh_help`），F9 那条遍历帮助页的测试照常通过。

**为什么好几个时不挑一个**：挑错了，这个 workspace 的会话会被派到另一台机器上的 Agent，可能是另一个人的登录、另一套 Provider；按字母序或"第一个"挑都是猜。被拒的代价是多敲一个参数。

**回归**：`workspace_add_takes_its_nodes_from_the_config_it_writes_into`（`crates/ccnm-cli/tests/cli.rs`），真实二进制，四段：一个 Agent 节点（`this = "hpsrv"`、`fodelf`）→ 写 `agent_node = "fodelf"`、`runtime_node = "hpsrv"`；两个（再加 `xdwmbp`）→ 拒绝、错误里有两个名字和 `--agent-node`、文件逐字节没变；`--agent-node xdwmbp` → 照写；默认节点名的配置 → `agent_node = "agent"`、不出现 `runtime_node`。

| 构建 | 结果 |
| --- | --- |
| 修之前（`8cf4d28` 上加用例） | 失败：`CCNM_E_CONFIG … workspaces.proj.agent_node = "agent" does not match any [nodes.*] entry / workspaces.proj.runtime_node = "runtime" does not match any [nodes.*] entry`，与真机一致 |
| 修之后 | 通过；原有的 `init_and_workspace_add_write_a_config_that_loads`、`a_name_that_is_taken_is_refused_with_something_to_type`、`adding_a_workspace_before_init_says_to_init`、`workspace_add_registers_a_root_it_may_not_look_at_and_says_so`、`every_visible_subcommand_is_described_in_chinese` 都通过 |

## 5. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载 25–37（另一个会话同时在跑 P68 的门禁）。测试在合入 P68 代码之后的 `1e4c2d3` 上跑，之后的提交只动文档和计划：

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 1.98.0 与 1.99.0（CI 的 stable）各一遍，通过 |
| `cargo +1.89 check --locked`（声明的最低版本） | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1072 通过、0 失败（P68 完成时 1068，本阶段新增 4 条） |
| `cargo test --workspace`（默认线程数） | 1072 通过、0 失败 |
| `python3 -B scripts/ci_gates.py` | 通过：计划检查、协议 46 + 29 个 fixture、Python 262 条 0 跳过 0 失败 |
| `python3 scripts/check_plan.py`、`git diff --check` | 通过 |

`ci_gates` 第一次跑是红的：`check_plan.py 退出码 1`，连带 `test_ci_gates` 里一条用例（它把仓库的计划文件拷进临时目录再跑一遍检查）。原因是那一趟跑到计划检查时，我刚把 P69 写进 ROADMAP、还没写进 status.json，两者正好对不上；计划补齐后不动文件重跑，全过。不是产品问题，也不是偶发用例。

## 6. 没覆盖的

- **真机**：三条都没有在 P62 的机器上复验（后注：F20、F24 已于同日晚复看通过；F21 仍没看）。F20 要一台旧构建的 Agent；F21 看一眼 Codex workspace 的 doctor 即可；F24 在节点另起名字的 Runtime 配置上跑一次 `workspace add`。都不花模型额度，可以并进 P62 续跑 F22/F23 复验的那一轮。
- **Linux**：没有在 Linux 上跑；改动不涉及平台相关的代码路径。
- **`--agent-node` 写了 `this` 自己**：会写出一个 Agent 与项目同机（colocated）的 workspace，配置校验放行，`ccnm run` 时按原来的规则拒绝（colocated 没有真实验收）。没有为它另加拒绝。
- **Codex 会话的审批本身**：见第 3 节，不在本阶段。
