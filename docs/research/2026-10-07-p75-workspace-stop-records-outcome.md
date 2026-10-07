# P75 按项目名停也记下"被停止"（2026-10-07）

**为什么做**：v0.12.0 换装冒烟时（[发版记录](2026-10-07-release-0.12.0.md)第 5 节），本机对日用 workspace `ccnm`（旧写法，配置里是 `agent_node`，没绑 Agent 实例）跑 `ccnm stop ccnm`：退出 0，fodelf 上 Agent 进程和 tmux 都没了，`mcp-serve` 退出，写锁 `released`——但 fodelf 上会话目录里没有 `stopping` 和 `exit`，`ccnm log` 里这条是"启动中"。前两轮（v0.11.1、v0.11.2）同一步用的是 `ccnm stop ccnm --session <id>`，记的是"被停止"。用户同日定：修。

## 1. 原因

`work::stop` 的交互停止只在请求点名了会话（`--session`）或实例（`--agent`）时才问 tmux 这个终端是哪个会话（`show-environment CCNM_SESSION`），然后写停止标志、杀终端、确认终端和通往 Runtime 的通道都没了、记结局。两样都没点名时，它只按名字杀掉 `ccnm-<ws>` 就返回。旧写法的 workspace 从 Runtime 发来的停止请求永远两样都不点名（`launcher::stop` 传 `None, None`）。

记录没有结局时，`ccnm log` 对交互会话（没有监督进程 pid）先算"启动中"，满 600 秒改算"没有结束记录"（`work::history`）。这几行的停止逻辑最后一次改是 `94eb7bf`，v0.11.2 以来没动过，不是 0.12.0 引入的。

## 2. 改法

不点名的停止也问终端它是哪个会话。`unselected_legacy_session` 只在下面几条都成立时认它：

- 终端报得出 id（更老的构建起的终端报不出）；
- id 是合法的会话名（它从终端环境里读出来、马上要拼成路径）；
- 这个 id 的记录读得出来，属于这个 workspace，没绑实例，是交互会话。

认下了就按 `--session` 那条路走：杀之前写停止标志，杀完确认终端没了、通道在 5 秒内退出，再记"被停止"（时长算到写停止标志那一刻）。确认不了时报的错和点名停止一样，只是第二行多一句：

```text
terminal ended but its Runtime MCP transport is still alive; state remains stopping
to record the stop once it has ended: ccnm stop <ws> --session <完整 id>
```

因为终端已经没了，下一次不点名的停止找不回这条记录——不给这一句，它会一直停在"正在停"。照抄这条再停一次，走的是已有的"点名、终端已没、有停止标志 → 记被停止"那条路。

认不下的（报不出 id、id 不合法、记录读不了、属于别的 workspace、绑了实例），和以前一样只杀不记。回给 Runtime 的报告里身份始终为空：Runtime 没选实例，回复里带了身份它会当成 `CCNM_E_VERSION` 拒掉（`launcher::verify_identity`）。

**行为变化**：旧写法 workspace 按项目名停一个认得出的会话时，现在也会等通道（最多 5 秒）；确认不了从以前的"退出 0"变成 `CCNM_E_NOT_READY`（退出 3），tmux 说杀了却还在时报内部错误——都和点名停止一致。多问 tmux 一次（`show-environment`）。点名停止、Machine API 的停止和协议都没动。

## 3. 测试

| 用例（`crates/ccnm-core/tests/session_identity.rs`） | 证明什么 | 旧代码上 |
| --- | --- | --- |
| `a_workspace_stop_records_the_legacy_session_its_terminal_names` | 不点名停旧写法会话：先写停止标志，结局是"被停止"，`ccnm log` 那一行是 `stopped`，报告里没有身份 | 红（没写停止标志） |
| `an_unconfirmed_workspace_stop_names_the_session_that_finishes_it` | 确认不了时报 NOT_READY、报错里有 `--session <id>`、留着停止标志没记结局；照着带 `--session` 再停一次，记成"被停止" | 红（旧代码返回成功） |
| `a_workspace_stop_records_nothing_it_cannot_tie_to_a_legacy_session` | 终端报不出 id、或 id 指向绑了实例的会话：照杀、不写任何东西、报告里没有身份、只调两次 tmux | 红（旧代码不问 tmux，只调一次）；"不写东西"那部分是护栏 |
| `a_workspace_stop_against_a_real_terminal_records_the_legacy_session` | 不假造任何东西：自己的 tmux server（不碰日用的 `tmux -L ccnm`）、真 `ps`，起一个带 `CCNM_SESSION` 的终端再按项目名停，真 tmux 确实答得出 id，记录是"被停止" | 红（840 行，没有结局）；没装 tmux 的机器跳过，CI 上没 tmux 算失败 |

按新行为改的旧用例一条：`work::tests::stopping_what_is_not_running_is_not_an_error` 给假 tmux 多备一次"找不到会话"——不点名的停止现在先问 id 再杀，两次都是 `can't find session`，结论（没在跑不算错）不变。

## 4. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，负载 14–34：fmt、clippy（1.98 与 `+1.99.0`）、`cargo +1.89 check --locked` 通过；`cargo test --workspace` 默认线程数 1096/0（v0.12.0 时 1092，新增 4 条），64 线程 1095/0（那一轮在加真 tmux 那条之前编译）；`ci_gates.py` 通过（计划、协议、Python 262 条 0 跳过）。没改协议、没改 Python。没在 Linux 上跑：改的路径不按平台分支，真 tmux 那条在线上 Linux CI 上也会跑，但这几个提交还没推送。

## 5. 没做的

- **没在真机上验**：要在日用机器上换候选构建，需另行授权。修法要 Agent 那一端是新构建才生效（记结局的是 Agent）。
- 不点名的停止确认不了时，也可以让下一次不点名的停止去找"这个 workspace 有停止标志、没结局"的记录，省掉抄 `--session`。没这么做：要扫会话目录、还得决定多条时取哪条，而这种情况本来就少（P62 量过，通道一般 5 秒内退）。
