# P73 doctor 的结论不再被"设计上不查"的行挡住（2026-10-07）

**现象**：任何真实配置跑 `ccnm doctor`，就算 0 项失败，最后一行也是"还不能用（N 项没查）"、退出 3。2026-10-07 发 v0.11.1 后，日用四个 workspace 都是"0 项失败，3 项没查"；P71 真机复验、P62 历次真机也都是这样。原因是网络隔离、本机工具策略两行永远是"没查"（SKIP），而 SKIP 一律挡结论。新用户读到的是"没配好"，实际上没有哪一项能再处理。

**证据范围**：离线测试，加一次只读的真机 doctor（第 3 节）。协议没变，没有调用模型，没有发版。

## 1. 规则

"没查"原来混着两种意思，现在分成两个状态：

| 状态 | 意思 | 挡不挡结论 |
| --- | --- | --- |
| **不查**（NOTE，新加） | 不管两台机器处在什么状态，这一行在这种配置下都只会是这个结果：不适用、超出只读检查能证明的范围，或者答案在同一张表的另一行 | 不挡 |
| **没查**（SKIP） | 该查、这次没查成，答案未知：对面没回答、对面构建太旧没报、前面一步失败了 | 挡，退出 3 |

改成"不查"的行：

| 行 | 为什么 |
| --- | --- |
| 网络隔离 | ccnm 不管出口策略，只读检查永远证明不了 |
| 本机工具策略 | 只有真开着的会话才说得清 |
| Codex 原生链（没开 `codex_exec_server`，或 Agent 不是 Codex） | 不适用 |
| 没有 Agent 的 workspace 里的 Agent 各行 | 没有 Agent，没什么可诊断的 |
| Runtime 上的项目（Operator 看不进执行账号家目录，且同一张表有执行账号回答的 `workspace 根目录`） | 答案在那一行，结论由那一行决定 |

**照旧是"没查"的**：上面那些情况以外的一切。特别是：外部 MCP workspace 里的 `Runtime 安全` 和 `exec_command`（执行账号的安全没人验，这正是不能算绿的）；同一个"看不进家目录"出现在没有 `workspace 根目录` 行的外部 MCP workspace 里；Agent SSH 失败后连带的那一串；旧构建没报的项。

**结论行**：没有失败、没有"没查"时是"可以用了"；有"不查"的行时写"可以用了（N 项不查，原因写在标"不查"的行里）"（英文 `READY (N not checked by design; each NOTE row says why)`），不让 READY 把它们藏起来。"还不能用（N 项失败，M 项没查）"里的 M 只数"没查"。退出码：只有正常、注意、不查时为 0。

中文选"不查"对"没查"，是因为"不"和"没"正好就是"本来不查"和"没查成"的区别。

## 2. 实现与测试

`crates/ccnm-core/src/doctor.rs`：`Status::Note`（不挡结论）、`Check::note`、结论行计数；上表五处改用 NOTE；`runtime_workspace` 多一个参数说明答案在不在同一张表里。`Status` 只在本机用，不跨机器传，加一个值不碍兼容。退出码的使用方只有 CLI 自己和 `scripts/deploy.sh`（只判 10），机器接口不调 doctor。

| 用例 | 证明什么 |
| --- | --- |
| `everything_good_is_ready_and_notes_what_doctor_does_not_check`（原 `everything_good_blocks_only_on_external_or_live_session_checks`） | 健康配置：READY、退出 0、那三行是 NOTE、没有 SKIP。**旧代码上红**：`NOT READY (0 failed, 3 not checked)` |
| `verdict_notes_are_ready_0_and_counted` | 只有 OK/WARN/NOTE 时 READY、退出 0，中英文结论行都注明数目 |
| `verdict_a_note_does_not_hide_a_skip` | 有一个 SKIP 就仍是 NOT READY、退出 3，计数只数 SKIP |
| `a_root_this_account_may_not_look_at_is_noted_only_when_answered_elsewhere` | `chmod 000` 父目录造出 F1 现场：同表有答案时 NOTE，没有时 SKIP（断言真跑了，没有走"本账号不受目录权限限制"的跳过分支） |

按新行为改写的旧断言（逐条都是这次有意改变的行为）：

- `an_external_mcp_only_workspace_is_diagnosed_not_called_a_bug`：Agent 各行 SKIP → NOTE；补断言 `Runtime 安全`、`exec_command` 仍是 SKIP，所以这个报告仍是 NOT READY。
- `the_exec_server_row_is_chosen_by_the_workspace_the_agent_and_the_preflight`、`the_exec_server_row_renders_in_both_languages`：不适用的两种 SKIP → NOTE（"没查" → "不查"）；"旧构建没报"那种仍断言 SKIP。
- `unreachable_work_fails_once_and_skips_the_rest`（doctor）与 `cli.rs` 对应用例：Agent SSH 失败时计数 13 → 11，少的就是那两行固定的。
- `doctor_says_it_could_not_look_instead_of_failing_the_project_row`（`cli.rs`，F1）：SKIP → NOTE。

## 3. 真机上看一眼

只读：用这次构建的 `target/debug/ccnm`（报 0.11.1）在本机对日用配置跑 `ccnm doctor ccnm`。两端仍是已装的 v0.11.1 发布版，只换了敲 doctor 的那个进程。

| | 结论行 | 退出码 |
| --- | --- | --- |
| 这次构建 | `可以用了（3 项不查，原因写在标“不查”的行里）`；三行"不查"是 Codex 原生链、本机工具策略、网络隔离 | 0 |
| 已装的 v0.11.1 | `还不能用（0 项失败，3 项没查）` | 3 |

这份日用配置另有 6 行"注意"（admin 组、可能读得到的私钥与 Agent 凭据、`allow_unconfined_exec`、`allow_unattended_exec`）：都是这台机器自己接受的风险，"注意"本来就不挡结论，这次没动。

## 4. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载 35–36。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`（1.98 与 `+1.99.0`） | 通过 |
| `cargo +1.89 check --locked --workspace --all-targets` | 通过 |
| `cargo test --workspace`（默认线程数与 `--test-threads=64`） | 各 1079 通过、0 失败（P72 后 1076，新增 3 条，改写 6 条） |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议、Python 262 条 0 跳过 |
| `python3 scripts/check_plan.py`、`git diff --check`；改过的文档锚点逐一核对 | 通过 |

## 5. 没做的

- 没发版。装着 v0.11.1 的三台在下一个版本之前仍是旧结论。
- 别的行怎么判没动；只在"没查"里分出一部分。
