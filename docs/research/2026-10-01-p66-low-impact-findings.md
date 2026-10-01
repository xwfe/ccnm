# P66 P62 真机查出的八条低影响项：F6–F12、F18（2026-10-01）

每条的真机现象见 [P62 记录](2026-09-30-p62-real-machine.md)第 5 节。全部是离线修复：先写在旧代码上失败的回归，再改；没有真机复验，没在本机装 LaunchAgent。`ccnm.machine/1` 没动；内部消息只加了两个可选字段（`StatusReport.other_instances`、`ProbeReport.rejected`），没加协议号。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P66.1 F6 别的实例占着项目终端 | `ccnm status <ws>` 问的是一个实例、项目终端却是另一个实例的会话时，多一行说是哪个实例、加什么参数看它；doctor 的 `Terminal session` 行同样说明 | `10e6247` |
| P66.2 F8 Controller 用非默认位置 | `ccnm controller install` 把 `--config`/`CCNM_CONFIG`（转成绝对路径）、`XDG_CONFIG_HOME`、`XDG_STATE_HOME`（只在 ccnm 认的时候）写进 plist，安装计划里逐个列出；三个都没设时 plist 与以前逐字节相同 | `5ada23e` |
| P66.3 F10 Agent 拒绝所选实例 | 探测报告带回拒绝原因；Operator 侧 doctor 报 `Agent selection` 失败并附原因，不再说 `identity differs`；Agent 侧不再把"没探反向链路"说成"在同一台机器上" | `9dfa695` |
| P66.4 F9、F18 | 中文帮助补上 `cleanup` 与 `mcp`、`controller` 的子命令和参数，加了一条遍历所有可见子命令的测试；`--print` 和 `ccnm result` 末尾写实际的 Agent 节点名 | `9afde85`、`70fc1ab` |
| P66.5 F7、F11、F12 | 三处 P62 当轮写的文档核对过，都在；F7 补一句"ccnm 不预写信任条目"及原因（2.5） | 见第 3 节文档提交 |
| P66.6 门禁与文档 | 见第 4 节 | — |

## 2. 各条改了什么

### 2.1 F6：问一个实例，答的却像是整个项目

一个 workspace 只有一个终端（tmux 会话）。`ccnm status <ws>` 不带 `--agent` 时按默认实例筛会话，P62 上 Codex 实例开着会话，问默认的 Claude 实例就得到"没有在跑的会话"——对实例来说是真的，对"这个项目现在有没有人在干活"来说是反的。

现在 Agent 那边筛会话时把被筛掉的那些会话的实例名带回来，人类输出变成：

```text
实例 claude-main 没有在跑的会话（--print 的运行不算在内）
这个项目的终端现在是实例 codex-main 的会话：加 --agent codex-main 看它
```

doctor 的 `Terminal session` 行同理：`no live session of this instance; <ws>'s terminal is a session of instance codex-main`。字段在没东西可说时不上线，旧构建的 Agent 不发它，Runtime 这边就照旧只说"没有在跑的会话"。

### 2.2 F8：Controller 在别处听着

launchd 起的 Controller 拿的是 launchd 的环境。P62 装的时候设了 `CCNM_CONFIG`、`XDG_STATE_HOME`，plist 里只有 `CCNM_LOG`：Controller 读默认配置、在 `~/.local/state/ccnm/controller.sock` 监听，install 在 `$XDG_STATE_HOME/ccnm/controller.sock` 上等满 10 秒报 `nothing is listening`。当时是手工改 plist 绕过去的。

| 变量 | 写不写 | 为什么 |
| --- | --- | --- |
| `CCNM_CONFIG` | `--config` 或 `CCNM_CONFIG` 给了就写，相对路径先转成绝对路径 | launchd 在 `/` 下启动 Controller，相对路径会指到别处。`--config` 本来就从 `CCNM_CONFIG` 取默认值，所以两种给法一样处理 |
| `XDG_CONFIG_HOME`、`XDG_STATE_HOME` | 非空且是绝对路径才写 | ccnm 自己就忽略空的和相对的（XDG 规范），照抄过去 Controller 也会忽略，写了只是噪音 |
| `PATH` | 不写 | 有意的：Controller 在 launchd 的 PATH 下找 Claude/Codex，那就是会话真正启动时的 PATH（plist 注释里写了）。P62 手工加 PATH 是为了用指定的 Codex 0.154.0，属于那次测试部署的偏离 |

安装计划里每个变量一行 `  with  变量=值`，`--dry-run` 先看得到。三个都没设时 plist 与以前逐字节相同，有一条单元测试把整份文本钉住，升级后重装不会让没挪过位置的机器出现任何差别。

验证只到 `--dry-run` 和单元测试：本阶段的停止点不许在本机安装或改动 LaunchAgent。CLI 黑盒测试在临时 HOME 下跑 `controller install --dry-run`，设相对的 `CCNM_CONFIG`、绝对的 `XDG_STATE_HOME`、相对的 `XDG_CONFIG_HOME`，确认前两个按规则进了 plist、第三个没进、等待的 socket 在 `XDG_STATE_HOME` 下；再在什么都不设时确认 plist 只有 `CCNM_LOG`。这条测试在旧代码上失败。

没改的：一个账号仍只有一个 Controller，Label 固定是 `dev.ccnm.controller`，带着别的位置再装一次会替换掉原来那个。要和日用的并存，仍然只能手工另起 Label（P62 在 fodelf 上就是这么做的）。

### 2.3 F10：拒绝的原因在路上没丢，是到了被丢的

P62 上 Agent 账号的 `~/.claude` 是 0755，Agent 拒绝了所选实例，探测报告里其实带着原因（Controller 那几行的错误就是它），但：

| 在哪跑 doctor | 以前 | 现在 |
| --- | --- | --- |
| Runtime Node（Operator） | `Agent selection  FAIL  CCNM_E_VERSION: Agent probe identity differs from the Runtime selection`，然后就没了 | `Agent selection  FAIL  CCNM_E_AUTH: the Agent Node refused this Agent before probing anything: <Agent 给的原因>`；Controller、Claude/Codex、反向 SSH 等行 `SKIP`，写明"没查：Agent 拒绝了所选实例"；`Agent SSH` 行 OK（连上了） |
| Agent Node | Controller 那几行带着原因，反向 SSH 行却说 `agent and project are both on agent, so nothing dials back` | 同上，不再有"同一台机器"这句 |

根子是被拒的报告没有身份、没有反向链路字段，而这两个"空"在读的一侧各自另有含义：没身份是旧式 workspace，没反向链路是项目和 Agent 同机。报告现在多一个可选的 `rejected`，明说"被拒了，原因是什么"；doctor 见到它就走专门的一支，不再读那些空字段。

**两端都要是 P66 的构建才看得到新的说法。** 旧 Agent 不发 `rejected`，新的 Operator 照旧只能比身份；旧 Operator 不认 `rejected`（解码时忽略），照旧说 `identity differs`。看到旧说法时到 Agent Node 上跑一次 doctor，Controller 那几行带着真正的原因——[排错手册](../troubleshooting.md)写了这一条。

### 2.4 F9、F18

- F9：P62 只看到 `cleanup` 一行是英文；查下去，`mcp probe`、`mcp bridge` 的参数和 `controller install/status/uninstall` 也缺中文。都补上，并加一条测试：递归读每一层 `--help` 的 Commands 段，每个可见子命令（`help` 除外）的说明里必须有中文字符。以后新加子命令忘了中文，这条测试就红。
- F18：`--print` 与 `ccnm result` 末尾那行原来写死 `session directory on work: …`。现在写实际节点名：Runtime 发起的写这个 workspace 解析出的 Agent 节点，在 Agent Node 上直接跑的写配置里的 `this`，没配时写 `this machine`。

### 2.5 F7、F11、F12：核对文档，以及 F7 为什么不预写信任条目

| 条目 | P62 当轮写在哪 | 核对结果 |
| --- | --- | --- |
| F7 首启提示 | [使用说明](../usage.md)"交互会话第一次起来，官方 CLI 会先问几句" | 两个提示（信任占位目录、Claude 的 auto mode 默认）各写了该怎么答、为什么；补了一句 ccnm 不替你提前答 |
| F11 外部会话被强杀后的输出 | [运维手册](../operations.md)"状态文件在哪，多大，怎么清"里 `output/` 那几条 | 写明 `mcp-serve` 被 `kill -9` 时后台命令继续跑、输出不会"连接一断就删"、`cleanup` 不列 bridge 会话、只能等 7 天或手动删；排错手册"后台命令跑着跑着就没了"一节的末尾与它一致。使用说明里清理那一段只链过去，按"一件事只写一处"不复述 |
| F12 dogfood 脚本说明 | `scripts/p12_dogfood_check.py` 的 `--read-only-workspace` 说明（`f25b4ab`） | 已改成"指向另一份克隆"，并说明重叠会被 `roots overlap` 拒绝；read 腿的 docstring 本来就解释了为什么只能用同一 workspace 的 read 模式 |

**F7 的"由 ccnm 预写信任条目"：不做。**

1. 信任记录在官方 CLI 自己的配置里。ccnm 不改官方 CLI 的配置文件（设计文档第 21 节：一个会改开发者自己 Claude 配置的工具，开发者没法推断它的行为），这也是本阶段的停止点。auto mode 那个提示更是这样——它问的就是要不要改全局默认，ccnm 起会话时自己传 `--permission-mode`，用不着那个默认。
2. 能不改文件、只在命令行上传的只有 Codex：0.154.0 的帮助里有通用的 `-c, --config <key=value>`（仓库 fixture `tests/fixtures/codex-0.154.0/help.json`），可以临时覆盖配置项。但"信任"对应哪个键、首启提示认不认命令行覆盖，fixture 里没有，也没实测过；受管适配器 pin 的 0.154.0 本机已经没有，Codex 账号额度到 10-04 才恢复。仓库规则是官方 Agent 的参数以实测版本和 fixture 为依据、不得猜参数，所以不传。
3. 代价是每个 workspace 第一次答一次，答过不再问；`--detached` 起的会话要先 attach 答完才连上工具，使用说明写了。

以后要做，先在 pin 的 Codex 版本上量出键名和行为、存成 fixture，再按 Provider 适配器的规矩加。

## 3. 用的人看得到的变化

- `ccnm status <ws>`：别的实例占着项目终端时多一行指过去（使用说明"单个项目"一节改了说法）。
- `ccnm controller install`：用了非默认配置或状态位置时，安装计划多几行 `with`，装出来的 Controller 在对的地方监听；运维手册"controller 不响应"一节加了这条和"一个账号一个 Controller"。
- doctor：Agent 拒绝所选实例时第一行就是原因；排错手册加了一节。
- `ccnm --help`（中文）：每个子命令都有中文说明。
- `--print`、`ccnm result` 末尾：`session directory on <实际节点名>: …`。

文档提交：使用说明、运维手册、排错手册、支持矩阵、README、P62 发现表与本记录。

## 4. 门禁

本机 macOS 26.6.2 arm64、rustc 1.98.0，机器负载 46–52（另有会话在跑）：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1057 通过（合并 mcp::jobs 修复后为 1048，本阶段新增 9 条） |
| `cargo test --workspace`（默认线程数） | 1057 通过 |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 个 fixture）、Python 262 条 0 跳过 |
| `python3 -m unittest tests.test_check_plan tests.test_check_protocol tests.test_ci_gates` | 48 条通过 |
| `git diff --check` | 通过 |

新增的 9 条回归在旧代码上都失败：F6 一条（`session_identity.rs`，旧代码只说没有会话）；F8 三条（两条单元测试改了 `plist`/`Plan::new` 的签名，旧代码编译不过；CLI 黑盒那条在旧二进制上 plist 里没有 `CCNM_CONFIG`）；F10 三条（doctor 两条旧代码分别报 `identity differs` 与 `are both on agent`，`work.rs` 一条旧代码没有 `rejected` 字段）；F9、F18 各一条（旧代码分别找到没有中文的子命令、末尾写 `work`）。没有动任何既有 fixture。

## 5. 没覆盖的

- **真机没有复验。** F6、F10 要两端都换成 P66 的构建；F8 要在真的 launchd 下装一次（本阶段不许在本机装），留给 P62 续跑，届时 fodelf 上并存的候选 Controller 仍要另起 Label。
- **F6 只看终端会话。** 别的实例的 `--print` 运行不在这一行里，和以前一样（status 本来就不数 print 运行）。
- **F10 的新说法要两端都是新构建**，见 2.3。
- **F7 不预写信任条目**，见 2.5。
- **F19 不在本阶段**：要用 P52 的方法在真机上同时采进程表和写锁，P62 续跑时做。
