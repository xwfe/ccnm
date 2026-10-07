# P70 doctor 不把旧 Agent 转述丢的字段算到 Runtime 头上、版本行写实际节点名（2026-10-07）

接 [P62.4 复验记录](2026-10-04-p62-4-recheck.md)第 6 节的 F25、F26。**只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0（clippy 另用 1.99.0）；没有连远端、没有跑模型。`ccnm.machine/1` 与内部协议号都没变，只改 Operator 本机 doctor 的判定与措辞。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P70.1 F25 | Agent 版本行不是 OK 时，Runtime 的版本号与本机相同就记成没比较、写明原因；版本号不同照旧失败；Agent 是同一个构建时判定不变 | `704cb9d` |
| P70.2 F26 | 版本行写 `the Agent Node <节点名> runs ccnm …`，不再写死 `work` | `704cb9d` |
| P70.3 门禁与文档 | 见第 4 节；排错手册 `5e8f320` | 本记录同一提交 |

## 2. F25：旧 Agent 转述的 Runtime 回答

**现象**（P62.4 真机）：fodelf 上的 0.9.0 读到的配置节点名正好对得上，doctor 走完全表。`Agent ccnm` 正确地报了版本不符，`Reverse SSH` 却说 `the Runtime Node reports ccnm 0.10.1 like this machine, but it is not the same build: it does not say how far its internal protocols go, so it is older than this build`——而 ccrun 跑的正是候选。

**原因**（这次有代码依据，不再是推断）：`Reverse SSH` 用到的 Runtime `hello` 是 Agent 探测时拿到、再放进它自己的报告里转给 Operator 的。v0.9.0 的 `HelloReport`（`git show v0.9.0:crates/ccnm-core/src/protocol/hello.rs`）没有 P64 加的 `wire` 字段，也没有 `deny_unknown_fields`：解析 Runtime 的回答时这个字段被静默丢掉，转出来就没有了，Operator 于是判 Runtime "没说自己能到哪一级内部协议，所以比我旧"。

**改法**（`doctor::probe_rows`）：先算 Agent 版本行，记下它是不是 OK。Agent 是别的构建时，它转述的内容里只有版本号字符串可信：

| Agent | Runtime 转述来的版本号 | `Reverse SSH` |
| --- | --- | --- |
| 同一个构建 | 任意 | 和以前一样（缺 `wire` 仍判 Runtime 不是同一构建） |
| 别的构建 | 与本机不同 | 和以前一样，失败 |
| 别的构建 | 与本机相同 | **SKIP**：`<节点> as <账号>, ccnm <版本>; build not compared: the Agent Node that relayed this runs another ccnm build and may have dropped what it does not know -- install the same build there first` |

记成没比较而不是 OK：这一行要回答"Runtime 是不是同一个构建"，这时答不了；整张表已经因为 `Agent ccnm` 失败，不会因此变绿。

## 3. F26：版本行的节点名

`Agent ccnm` 的两处调用把节点写死成 `"work"`（P66 的 F18 修过 `--print` 与 `result` 末尾同一类问题，这两处漏了）。测试里 Agent 节点的 ssh 别名正好叫 `work`，所以断言一直对得上。改成按配置里的节点名写 `the Agent Node <节点名>`，测试断言跟着改。

## 4. 验证

**回归** `an_old_agent_relaying_the_runtime_does_not_make_the_runtime_look_old`（`doctor::tests`）：探测报告带对得上的身份、Agent `hello` 是 0.9.0 且没有 `wire`、Runtime `hello` 是本机版本且没有 `wire`。

| 构建 | 结果 |
| --- | --- |
| 修之前（`7e89ae3` 上加用例） | 失败；渲染出的两行与真机原文一致：`Agent ccnm FAIL … work runs ccnm 0.9.0 …`、`Reverse SSH FAIL … not the same build … older than this build` |
| 修之后 | 通过；同一用例另守两条：旧 Agent 转述的版本号不同（0.8.0）仍失败；同一个构建转述时缺 `wire` 仍判 Runtime 不是同一构建 |

**门禁**（本机负载 22–35）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`（1.98 与 `+1.99.0`） | 通过 |
| `cargo test --workspace`（默认线程数与 `--test-threads=64`） | 各 1073 通过（P69 后 1072，新增 1 条） |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 fixture）、Python 262 条 0 跳过 |

## 5. 没覆盖的

- 真机没复验。要一台旧构建的 Agent，且它读到的配置节点名与 Operator 的一致（P62.4 用 fodelf 上 0.9.0 的备份配本轮配置做到过），不花模型额度。
- 只处理了 `Reverse SSH` 这一行。旧 Agent 转述的其他内容（Runtime 的安全审计、MCP 握手）若在后来的版本里加了字段，同样可能被丢；它们现在没有按"字段缺失"下结论的判定，所以不受影响。
