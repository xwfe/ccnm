# P78：不要求专用执行账号，没写 `runtime_user` 就按共用账号跑（2026-10-07）

## 结论

Runtime 节点的配置里**没写 `runtime_user`，就是共用账号**：Agent 经 SSH 登进来的是哪个账号，命令就以谁的身份跑，通常就是用户自己。隔离相关的检查照做、照显示，但从"失败"降为"注意"，不挡会话、不挡 `exec_command`、`call_mcp_tool`，命令结果也不再带"未隔离"那一行。**写了 `runtime_user` 就是专用账号模式，判法和 P78 之前一字不差。** 用户 2026-10-07 定：默认不要求 `ccrun`，专用账号只在文档的安全章节介绍。

| 情况（Runtime 上的账号） | P78 之前 | P78 起，没写 `runtime_user` | P78 起，写了 `runtime_user` |
| --- | --- | --- | --- |
| 没写 `runtime_user` 本身 | `Runtime user` FAIL，`exec_command` 被拒 | `Runtime user` WARN，说明是共用账号、要隔离去哪看 | — |
| 在 admin / wheel / sudo 组、有免密 sudo、能写 Docker socket、`~/.ssh` 里有私钥 | FAIL，`exec_command` 被拒（`allow_unconfined_exec` 可放） | WARN，不挡 | FAIL，同 P78 之前 |
| 读得到 `~/.claude` / `~/.codex` 里的登录（或说不清） | FAIL，整个会话不开（`allow_unisolated_credentials` 可放） | WARN，不挡；`exec_command` 前那次复查也不挡 | FAIL，同 P78 之前 |
| 以 root 运行 | FAIL，`exec_command` 被拒 | 同左 | 同左 |
| 说不清执行身份是谁 | FAIL，什么开关都放不开 | 同左 | 同左 |
| 环境里继承了 `ANTHROPIC_*` / `CLAUDE_*` 等认证变量 | FAIL，什么开关都放不开 | 同左 | 同左 |

## 为什么用 `runtime_user` 当开关

- 它本来就是"我建了专用账号，替我核对它"的意思：写了才有东西可比。P78 之前没写它就判失败，理由是"分不清这是专用账号还是开发者自己的"——现在默认就当作开发者自己的。
- 不用加新配置项：已经建了 `ccrun`、写了 `runtime_user` 的部署行为不变。
- 忘了写 `runtime_user` 的专用账号不会"悄悄不查"：`Runtime user` 一行写明当前是共用账号模式、要隔离写什么。

## 为什么是降级而不是不查

那几项照查，是因为它们回答的正是用户该不该建专用账号的问题：这台机器上的这个账号，模型跑的命令够得到什么（私钥、admin、Agent 登录）。不查就没人告诉他。降到 WARN 而不是 NOTE（"不查"）：这些项查过了，性质确实不成立，只是这个模式下不挡。

root、身份不明、继承认证环境三条不跟着降：前一条和是不是专用账号无关；后两条本来就是"任何开关都放不开"，修法也和账号无关（别以 root 跑、修好 `id`、别 export）。

## 改了什么

- `safety::audit`：`expected_user` 为空时，sudo、admin 组、SSH 私钥、Agent 登录、Docker 五类发现里的 FAIL 改成 WARN；`Runtime user` 那行由 FAIL 改成 WARN 并说明模式。`Audit` 加可选字段 `shared_account`（为 `false` 时不写），Runtime 交给 doctor 的体检结果里带着；旧 Runtime 不带，读成 `false`，正是它当时的判法。
- `credentials::runtime_gate`（`exec_command`、`call_mcp_tool` 起程序前的复查）多一个参数，共用账号时不因 Agent 登录拒绝；环境检查不变。
- doctor 的 `exec_command` 行在共用账号时写"允许：命令以 X 的身份跑，共用账号（没写 runtime_user），它能碰的都能碰"，不再说"账号是受限的"。
- `ccnm ws add --allow-unconfined-exec` 的帮助改成"写了 runtime_user 时……；没写时只有以 root 运行才需要它"。
- 没写配置文件（`CCNM_CONFIG` 指向不存在的文件）时同样是共用账号：P78 之前它和"没写 `runtime_user`"一样判失败，现在一样放行。能起 `internal mcp-serve` 的人本来就能以这个账号 SSH 进来执行任何东西，这一步不多给什么。

## 用例

新增 5 条（Rust 4、中立 MCP 客户端 1），改写 1 条、调整 6 条；没调整之前，7 条旧用例在新代码上红过一次，确认行为确实变了：

- 改写：`safety` 的 `no_configured_runtime_user_is_itself_a_failure` 改成 `no_configured_runtime_user_is_a_shared_account_that_is_shown_not_refused`（同一个有 admin、免密 sudo、私钥、Claude 登录的账号，不写 `runtime_user` 全是 WARN、会话与命令都放行，写了全是 FAIL）。
- 新：`safety` 的 `a_shared_account_still_refuses_root_an_unknown_identity_and_inherited_auth`；`doctor` 的 `a_shared_account_is_ready_and_says_it_is_not_isolated`；`mcp_read_file` 的 `a_runtime_without_runtime_user_runs_commands_as_a_shared_account`（真实二进制，HOME 里放了 Codex 登录，命令照跑、结果里没有 NOT confined）；`external_mcp` 的 `a_login_in_a_shared_accounts_home_does_not_refuse_the_session`；Python 中立客户端的 `test_a_runtime_without_runtime_user_is_a_shared_account_that_runs_commands`。把 `shared_account` 临时恒设为 `false` 跑一遍，改写的那条和 `mcp_read_file`、`external_mcp` 两条新用例都红（`doctor` 那条用的是手写的体检结果，不经审计；root 那条钉的是没变的行为）。
- 调整：原来靠"不写 `runtime_user` 必然失败"造不隔离环境的用例（`mcp_read_file` 的 `config_for`、`external_mcp` 的 `shared_home` 改名 `login_in_home`、`test_remote_workspace_mcp.py` 的 `write_config`、`safety` 里两条拒绝文案用例），改成写一个测试不会用的账号名当 `runtime_user`。不用当前用户名，是因为在真用 `ccrun` 跑测试的 Linux 机器上（hpsrv 的回归就是这样跑的），那个账号是真隔离的，用例会失去"不隔离"这个前提。`provider_tests` 的凭据复查用例补一个共用账号的断言。

## 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0：`cargo fmt --check`、`cargo clippy --workspace --all-targets -D warnings`、`cargo +1.89 check --locked` 通过；`cargo test --workspace --no-fail-fast` 1101 passed / 0 failed（P77 时 1097）；`python3 -B scripts/ci_gates.py` 通过（计划、协议 46 + 29 个 fixture、Python 263 条 0 跳过）。

## 文档

生产安全开头改成"默认就用你自己的账号"，新增"要不要建专用账号"一节，作为讲 `ccrun` 的唯一入口；快速开始删掉"建 Runtime Service Account"那一步和"项目和 Claude 登录在同一个账号下"那一节，换成一段"标注意的几行"；README 不再把建 `ccrun` 当必经步骤，并写明 P77、P78 还没发版；配置说明的 `runtime_user`、两个 `allow_*` 开关按两种模式改写；支持矩阵、运维、排错、Provider 安全、架构、使用说明措辞同步；Remote Workspace MCP 契约加一条带日期的说明（协议没变，Runtime 门禁的默认判法变了），第 4.5 节措辞改；AGENTS.md 和 ROADMAP"不可漂移的原则"里关于执行身份的条目改写。

## 没做的

- 没在真机上跑：没有一台 Runtime 以不写 `runtime_user` 的方式、用 P78 的构建起过会话。
- 没在 Linux 上跑这轮测试（线上 CI 会跑）。
- 没动任何机器上已装的 ccnm 或配置；没发版。v0.12.0 仍是旧判法。
