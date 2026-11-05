# P118：删掉 Codex Agent（2026-11-05）

结论：ccnm 只起 Claude Code 会话了。配置里写着 `provider = "codex"` 的，整份配置解析失败，报 `CCNM_E_CONFIG`，并说清怎么改。外部 MCP 入口（gld hub 用的 `ccnm mcp bridge`）、`ccnm rpc`、Windows Runtime 都没动。这一轮只有离线证据，没跑真机。

## 用户定的范围

2026-11-05：发大版本，完全去除 Codex，只做 Claude Code。追问时用户说"只删除 codex，其他不动"，因为还要和 gld 联动。gld hub 连 ccnm 走的是外部 MCP 入口（`ccnm mcp bridge`），不经过 Codex Agent。gld 的 RFC-0009 打算以后经 `ccnm.machine/1` 往远端成员派活（D4，还没做），这条路以后只能派 Claude Code。

## 删了什么

- **代码**：`crates/ccnm-core/src/provider/codex/` 整个目录（5 个文件，2191 行）；`AgentProvider::Codex` 和只为它走的分支（起会话、`-c`、`--print`、结果解析、doctor、额度、Controller、自动接回、交接）；`[agents.*]` 的 `model`（只有 Codex 认）；会话记录的 `ask_before`（只有 Codex 读）；`AgentResult::Codex`；额度里只有 Codex 报的字段（`limit`、`minutes`、`resets_at` 和两种 note）；`Redaction::Withhold`；`login_caveat`；`paths::codex_home`。`crates/` 下共 49 个文件，+677 / −4118 行；其中非测试源码 41 个文件，+520 / −3447 行。
- **测试、夹具、脚本**：`crates/ccnm-core/tests/codex_measurements.rs`、`scripts/measure_codex.py` 和它的测试、`tests/fixtures/codex-0.153.4/`、`tests/fixtures/p3-public-codex/`，以及 `tests/fixtures/codex-0.154.0/` 里除沙箱那份以外的录制。共 89 个文件，+174 / −2916 行。以前拿 Codex 当"第二个实例"的测试，改成用第二个 Claude 实例（`claude-second`），原来覆盖的东西没少。
- **文档**：18 个文件，+187 / −320 行。研究记录不动。

## 留下什么、为什么

- **`exec_sandbox = "codex"`**：借 `codex sandbox` 给 `exec_command` 套 OS 沙箱。受管 Claude 会话和外部 MCP 会话都走这一个 `exec_command`，删了就没有 OS 沙箱了。版本检查和 `CODEX_VERSION`（0.154.0）从 provider 搬到了 `crates/ccnm-core/src/mcp/sandbox.rs`，测试跟着搬了过去。
- **读给 Codex 装的 MCP server 和 skills**：两边都读 `~/.codex/config.toml`（这是共享库 toexec-mcp 的行为，gld 也这么读），也读 `~/.codex/skills`。这里只是读配置，不跑 Codex。
- **凭据保护**：`provider::CODEX_CREDENTIALS` 和 `KNOWN_CREDENTIALS`。项目机器的账号上装没装、登没登录 Codex，跟 ccnm 起不起 Codex 会话无关，模型的命令都不能碰到那份登录。改的过程中这里漏过一次：安全检查有四处还在遍历 `AgentProvider::ALL`，而它这时只剩 Claude，于是 doctor 不再查 Codex 登录，命令环境也不再按 `CODEX_`、`OPENAI_` 前缀清掉变量。`safety.rs` 的 `codex_credentials_are_checked_even_when_claude_is_the_default` 因此变红；改成遍历 `KNOWN_CREDENTIALS` 后转绿。
- **`AgentProvider` 这个类型**：只剩一个值也留着。配置、会话记录、线上消息里都有 `provider` 字段，读到 `codex` 要点名拒绝（`provider::CODEX_REMOVED`），不能当成 Claude 读下去。

## 配置里写着 codex 会怎样（P118.2）

拿本机 `target/debug/ccnm` 配一份带 `[agents.codex-main] provider = "codex"` 的配置，敲 `ccnm ls`，退出码 10：

```text
CCNM_E_CONFIG:
…/codex-config.toml: TOML parse error at line 10, column 12
   |
10 | provider = "codex"
   |            ^^^^^^^
provider "codex" is no longer supported: since 1.0 ccnm starts Claude Code sessions only. Delete the [agents.*] or [profiles.*] table that names it and point the workspaces using it at a Claude instance; the Codex login directory is left in place for you to delete
```

- `profiles.toml` 里写 `codex` 也报这一句。这个文件的解析错误原来一律隐藏内容（怕带出私有路径），这一句不引用文件里的任何东西，所以单独放了出来。
- 先红后绿：`a_codex_instance_is_refused_by_name`、`a_codex_profile_is_refused_by_name` 两条用例放进改动前那个提交（`e0d32bb`）的副本里跑，2 条都挂（旧代码照常解析成功）；改动后都过。
- 以前 Codex 会话留下的记录（会话目录里的 spec 写着 `codex`）：`ls`、`status`、`log`、`cleanup` 这类列表命令读不了就跳过（代码里是 `.ok()`、`let Ok … else`）；`ccnm result` 正好选到这样一条时报上面那句。这一条是读代码得出的，没在真机上跑。
- ccnm 以前给 Codex 建的登录目录（默认 `~/.config/ccnm/agents/codex`）不会自动删，排错手册写了怎么手动删。

## 动了哪些共享文件

`crates/ccnm-cli/src/main.rs`，以及 `crates/ccnm-core/src/` 下的 `config.rs`、`controller.rs`、`doctor.rs`、`instance.rs`、`instance/profiles.rs`、`lang.rs`、`launcher.rs`、`mcp/agent_mcp.rs`、`mcp/agent_skills.rs`、`mcp/output.rs`、`mcp/sandbox.rs`、`mcp/server.rs`、`on.rs`、`overview.rs`、`paths.rs`、`process.rs`、`protocol/mcp.rs`、`protocol/run.rs`、`provider/claude/mod.rs`、`provider/claude/usage.rs`、`provider/context.rs`、`provider/mod.rs`、`provider/result.rs`、`quota.rs`、`rpc/mod.rs`、`safety.rs`、`safety/credentials.rs`、`safety/environment.rs`、`session.rs`、`session/carry.rs`、`session/transport.rs`、`session/view.rs`、`ssh.rs`、`systemd.rs`、`work.rs`。其中一半以上只改了注释或测试。

## 验证

本机 macOS arm64，提交 `6d4691d`（代码）和 `cb5ce8f`（文档）：

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：干净。
- Windows 目标的三条 clippy（命令见 `docs/development.md`）：干净。
- `cargo test --workspace --no-fail-fast`：1207 过、0 挂。0.22.0 发版时是 1259 条，少掉的是删掉的 Codex 用例。
- `python3 -B scripts/ci_gates.py`：通过。计划检查 113/114；协议 46 + 31 个 fixture；Python 249 条跑、0 失败、0 跳过。

没做的：真机（零额度也没跑）、换装、CI 线上运行。
