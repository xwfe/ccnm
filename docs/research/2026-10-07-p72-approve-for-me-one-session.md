# P72 会话里选过 Approve for me 不再延续到之后的受管 Codex 会话（2026-10-07）

修 [P71 真机复验](2026-10-07-p71-real-machine-recheck.md)第 5 节查出的 F27。

**证据范围**：离线测试加 Codex 0.154.0 零额度实测（假模型）。**没有调用真实模型、没有在真机上复验、没有发版**——装着 v0.11.0 的 Agent 仍有 F27，要等下一个版本。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P72.1 实现 | 走 MCP 的受管 Codex 交互会话，启动参数多一项 `-c approvals_reviewer="user"`，放在沙箱与审批策略旁边。print 会话、exec-server 链不变 | `f1b0110` |
| P72.1 零额度实测 | profile 里写着 `auto_review` 时：不带这一项，`exec_command` 一次都不问人、交给 Codex 的自动审查；带上之后每次都问，放行的到了 server、取消的没到 | 本记录 |
| P72.2 doctor 与文档 | `Command approval` 对 Codex：会话里切走"只管那一个会话，下一个会话照样问"；用户文档写清旧 Agent 仍会延续、怎么去掉 | `420f886`；文档本记录同一提交 |
| P72.3 门禁 | 见第 4 节 | — |

## 2. 改了什么、为什么只改这一处

交互会话的 `CODEX_HOME` 就是 Agent 上的 Codex profile 目录（默认 `~/.config/ccnm/agents/codex/`），Codex 启动时读那里的 `config.toml`。ccnm 早就把沙箱（`--sandbox read-only`）和审批策略（`approval_policy="on-request"`）写在命令行上，命令行盖过配置，所以会话里切 Full Access 只管当前会话。审批由谁来答（`approvals_reviewer`）没写，而 Codex 把 `/permissions` 里的 Approve for me 存成 `approvals_reviewer = "auto_review"`，于是一次选择延续下去。0.154.0 源码里这一项只有 `User` 和 `AutoReview` 两个值（`codex-rs/tui/src/chatwidget/permissions_menu.rs`）。

| 会话 | 读不读 profile 的配置 | 这次 |
| --- | --- | --- |
| 交互（MCP） | 读 | 加 `-c approvals_reviewer="user"`，`ask_before` 为空（workspace 开了 `allow_unattended_exec`）时也加：审批由谁来答不该由一份会被界面改写的文件决定 |
| print | 不读（`codex exec --ignore-user-config`），而且 `approval_policy="never"` 根本不问人 | 不变，Codex 兼容性 fixture 不用重录 |
| exec-server 链（已封存） | 不读：用 ccnm 给每个会话生成的 `CODEX_HOME` | 不变 |

会话里的人仍能用 `/permissions` 把当前会话切到 Full Access 或 Approve for me，ccnm 拦不住；Codex 也照样把选择写进 profile（零额度实测：带着这一项启动，配置文件里的 `auto_review` 原样留着）。区别只在于下一个会话不再继承它。

## 3. 实测与测试

**零额度实测**（夹具 `docs/research/probes/p71-codex-approval.py`，Codex 0.154.0 按 GitHub release 的 digest 核过 sha256，假模型，Code Mode，交互界面在单独的 tmux server 里）。两种情况都先在临时 `CODEX_HOME/config.toml` 里写一行 `approvals_reviewer = "auto_review"`（模拟 F27 现场），再加 P71 的 `-c mcp_servers.ccnm.tools.exec_command.approval_mode="prompt"`：

| 情况 | 交互界面 | 探针 server 收到 |
| --- | --- | --- |
| 不加（v0.11.0 的启动参数） | 不问人；每次 `exec_command` 都由自动审查判，假模型答不出合法 JSON，两次都被拒（`Automatic approval review failed: guardian assessment was not valid JSON`）。真实模型下同一档是放行（P71 真机复验第 4 节） | 只有 `read_file` |
| 加 `-c approvals_reviewer="user"`（P72 的启动参数） | 每次都弹 `Allow the ccnm MCP server to run tool "exec_command"?`；第一次 Allow、第二次 Cancel，模型拿到结果和 `user cancelled MCP tool call` | `read_file` 与第一次 `exec_command` |

做法：两种情况各起一份 `serve-model <目录>`，在 `<目录>/home/.codex/config.toml` 预先写好那一行，再用 `cmd <目录> <codex> -c …` 打出启动命令放进 tmux。

**测试**：

| 用例 | 证明什么 |
| --- | --- |
| `an_interactive_session_does_not_let_the_profile_choose_who_approves`（`provider::codex::tests`） | 交互会话（`ask_before` 有无都算）恰好一处 `-c approvals_reviewer="user"`；print 会话没有。旧代码上红：交互参数里没有这一项 |
| `the_approval_row_says_how_codex_sessions_ask`（`doctor::tests`，补一句） | Codex 那一行写明 "the next session asks again"；旧文本里没有这句 |

## 4. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载约 37（10 核）。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`（1.98 与 `+1.99.0`） | 通过 |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `cargo test --workspace`（默认线程数与 `--test-threads=64`） | 各 1076 通过、0 失败（P71 后 1075，新增 1 条，doctor 用例补了一句断言） |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议、Python 262 条 0 跳过 |

## 5. 没覆盖的

- **真机与发版**：没做。日用两台和 hpsrv 装的 v0.11.0 仍有 F27；在那之前，旧构建上去掉的办法见[排错手册](../troubleshooting.md#受管-codex-会话exec_command-每次都弹或者一次都不弹)。
- Approve for me 在当前会话里怎么判，仍是 Codex 自己的事，ccnm 不管。
- 只测了 Codex 0.154.0。
