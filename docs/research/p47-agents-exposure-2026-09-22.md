# P47：按执行账号的 `~/.agents/mcp.json` 收窄工具和 skills（2026-09-22）

用户要"从 `~/.agents/mcp.json`、从 `~/.agents` 细粒度管理 MCP / skills 的暴露"，并在三层里只选了第一层：gld、ccnm 按这个文件决定**自己**给哪些工具、skill 给到哪一档；不聚合别的 server，不让受管会话接别的 server。格式、四档的来历、为什么这样定在 toexec 的 [RFC-0001](https://github.com/xwfe/toexec/blob/main/docs/rfc/0001-agents-exposure-policy.md)，解析在 `toexec-agents` 0.1.0。这里只记 ccnm 这边怎么接、为什么。用法见[配置说明](../configuration.md#agentsmcpjson再关掉一些工具和-skills)。

## 怎么接的

| 位置 | 做法 | 为什么在这里 |
| --- | --- | --- |
| `Server::new` | 在执行门和写锁之后读一次，存进会话 | 文件是执行账号 HOME 下的，读它的就该是执行账号的进程；工具表只发一次，中途重读只会得到一份和模型手上不一致的表 |
| `offers()` | 原来按 `external_mcp` 模式判断，现在再问一次文件 | `tools/list` 和 `get_tool` 都从这里取，不会两边说法不同 |
| 手写的 `call_tool` | 被关的工具在进路由之前就拒，`isError` + `CCNM_E_POLICY`，写明哪条规则 | 原来只有 4 个工具的处理函数自己检查（`read` 模式扣掉的那几个），文件却能关掉任何一个；客户端也可能缓存着旧表 |
| `skills::discover_for` | 发现之后按档收窄：`off` 拿掉，`user-invocable-only` 置 `model_invocable = false`，`name-only` 去掉描述 | 目录、列表、`load_skill`、prompts 都从这里取，四处意思一样；只会把标志往下拉，所以写 `on` 放不开 frontmatter |
| `workspace_info` | 结果里、`[server pid …]` 那行之前加两行：关掉了什么、哪些名字不是这里的工具 | 结构化字段属于冻结契约，不加字段；probe 按最后一行解析，插在前面不影响它 |
| doctor | Runtime 那一半加一行"暴露规则" | 见下 |

**文件写坏时会话打不开**（`CCNM_E_CONFIG`，带位置）。退回"不收窄"等于把想关的又打开了，而且用户多半不会发现。工具名写错不拦会话——名字随版本增减，一个旧名字让所有会话打不开代价太大——但写错在 `disabledTools` 里意味着想关的还开着，所以 doctor 当失败报、`workspace_info` 也列出来。

**doctor 那一行只在敲命令的就是执行账号时才读文件**：配了 `runtime_user` 且当前账号（`/usr/bin/id -un`）不是它，就跳过并说该去哪看。读操作员自己的 `~/.agents/mcp.json` 然后说成 Runtime 的，就是 P7.3 撞过的那类"答了没人问的问题"。这一行因此在配了 `runtime_user` 的机器上多一次本地 `id -un`；一条原本断言"只跑了一次本地子进程"的测试照实改了，理由写在测试里。

**不是对模型的约束**：执行账号能写自己的 HOME，开着 `exec_command` 的会话里模型能改这个文件、影响下一个会话，和它能改 `config.toml` 一样。要拦模型，用 `external_mcp` 和执行门。

**没做**：Runtime 执行账号的 `~/.agents/skills` 不发现（原生 CLI 会读用户级 skills，但 ccnm 的目录挤在 2048 个 UTF-16 码元的工具描述里，66 个用户级 skill 会把项目自己的挤掉，专用执行账号的 HOME 下通常也没有个人 skills）。受管会话 Agent 一侧的放行清单没改：Runtime 没列的工具，Agent 放行了也调不到。

## 测试

新增 7 条：`exposure` 模块 2 条；skills 1 条（四档分别在目录、列表、`load_skill`、prompts 上，`ccnm` 条目盖过顶层、`gld` 条目不影响 ccnm、`on` 放不开 frontmatter）；server 1 条（内存管道上真的发 `tools/call`：不列、硬调被拒并写明规则、`workspace_info` 列出关掉的和写错的且 probe 仍解析得出 server 行）；doctor 1 条；真实二进制 2 条（`mcp_read_file.rs`：执行账号 HOME 下有文件时 `tools/list` 少了 `exec_command`、硬调被拒、`off` 的 skill 从目录和 prompts 消失；文件写坏时进程非零退出、stdout 一个字节都没有、stderr 带 `CCNM_E_CONFIG` 和位置）。

既有测试只动了一处断言（doctor 那条，理由见上）和 skills 测试模块里两个同名包装函数（让旧调用不带策略参数）。没有 fixture 重录：不写文件时行为逐字节不变，冻结契约的两份 `tools-list-*.json` 照旧通过。

## 门禁

依赖用一次性环境变量从本地 toexec 解析（tag `toexec-agents-v0.1.0` 还没推送）：

```bash
CARGO_NET_GIT_FETCH_WITH_CLI=true GIT_CONFIG_COUNT=1 \
GIT_CONFIG_KEY_0=url.file:///Users/bing/xdw/toexec.insteadOf \
GIT_CONFIG_VALUE_0=https://github.com/xwfe/toexec.git cargo test --workspace
```

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 905 passed / 0 failed（P46 时 898，多的 7 条是本阶段新增） |
| `cargo +1.89 check --workspace --all-targets --locked` | 通过 |
| `python3 -m unittest discover -s tests` | 194 passed |
| `check_plan`、`check_protocol`（38 + 29 个 fixture）、`git diff --check` | 通过 |

只在 macOS arm64。编译时本机盘满过一次（剩 119 MiB，rustc 报 `No space left on device`），删了三个仓库的 `target/debug/incremental`（共约 20 GB 的增量缓存）后用 `CARGO_INCREMENTAL=0` 重跑。

## 没验的

真实 Host（Claude Code、Codex）缓存了旧工具表时怎么显示被拒；Linux；真实模型。
