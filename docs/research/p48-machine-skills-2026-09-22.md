# P48：两台机器上装好的 skills 交给会话（2026-09-22）

零额度，macOS arm64。实测脚本和结果在 toexec 仓库的 `evidence/v3-parity/machine-skills/`（toexec 6741340）；这里只记 ccnm 按它做了什么决定。怎么开关见[配置说明](../configuration.md#machine_skills)，协议上的变化见[协议第 5.1 节](../protocol/remote-workspace-mcp-v1.md#51-load_skill-与-prompts项目自带的-skillsp36-新增)。

编号说明：P47 被同一天做完又整个撤销的"用 `~/.agents/mcp.json` 收窄自身工具"占用过（撤销提交 `37694f6`），不再用，这一阶段从 P48 起。

## 起因

用户 2026-09-22 的要求：gld 和 ccnm 都能用上 **Agent 机器和 Runtime 机器上已经装好的** skills，而不只是项目里自带的；ccnm 默认全开；共用代码进 toexec；模块化，不要了能整块删掉。并且问：经 Runtime 传的时候会不会丢信息、数据量会不会太大。

P36 起 ccnm 只交项目里的 skills（`load_skill`），执行账号 HOME 里的不读；Agent 那边，P46 把原生 `Skill` 放进了拒绝表。

## 先量的四件事

Claude Code 2.1.278、Codex 0.154.0（0.155.1 相同）连本机假模型接口，启动参数照 ccnm 拼，临时 HOME 里放探针 skill。

| 问题 | 结果 | 对设计的影响 |
| --- | --- | --- |
| 受管会话里原生会不会列出 Agent 上装的 | Claude：`Skill` 在拒绝表时一个都不列。Codex：**已经列了** `~/.agents/skills`、`$CODEX_HOME/skills` 和它自带的 5 个，给的是 Agent 上的路径，要靠 shell 去读——而 ccnm 关了 shell | Codex 那份清单是误导，要关（`-c skills.include_instructions=false`，实测整段消失） |
| 放开原生 `Skill` 行不行 | 正文拿得到，附件要 `Read`；只对 skills 目录放开 `Read` 时，**会话的工作目录不用 allow 也读得到**——那是 ccnm 给这个会话记状态的目录（`mcp.json`、payload）。还会带上 Claude Code 自带的、假定 Read/Bash 都在的 skill；`~/.agents/skills` 不读 | Agent 上的 skills 由 ccnm 自己读出来交出去，原生的继续关 |
| 同名时谁赢 | 用户目录的赢，项目的被盖掉（项目 skills 是读了的） | 合并 Runtime 上的清单照这个来 |
| ccnm 做出来的服务接上真实客户端 | Claude：工具在表里、print 模式不被权限拦、读得到附件、`.env` 被拒。Codex（`gpt-5.1-codex`）：`mcp__ccnm_agent` 调得通，自带清单消失 | 做法成立 |

## 做了什么

**Runtime 这台**（`mcp/machine_skills.rs` + `mcp/skills.rs`）：`load_skill` 同时扫执行账号 HOME 的 `~/.claude/skills`、`~/.agents/skills`、`~/.codex/skills`、`~/.claude/commands`。排在项目的后面；同名时装好的赢，被盖掉的写进 `Not offered` 并说明被谁盖掉。符号链接跟着走（skills CLI 的装法），但真实目录是 HOME 本身或文件系统根的不收。同一个 skill 经链接或逐字节相同的拷贝出现第二次时静默去掉。

**Agent 这台**（`mcp/agent_skills.rs`）：`ccnm internal agent-skills --payload …`，一个只读的 MCP server，由 Claude Code / Codex 在 Agent 上直接起、不经 ssh；只有一个工具 `load_skill`（到模型那里是 `mcp__ccnm_agent__load_skill`）和同样的 prompts。代码和 Runtime 那个是同一份，换一个"只有本机"的范围和一套说"不在项目机器上"的措辞。会话创建时（`session::create`）按 Agent 自己配置决定起不起：Claude 写进 `mcp.json` 的第二个 server 并在 settings 里 allow；Codex 在启动时读会话目录里的 `agent-skills.json`，加四条 `-c mcp_servers.ccnm_agent.*`。Codex 远端会话**不论开关**都加 `-c skills.include_instructions=false`。

**读文件**：`load_skill` 加两个可选参数 `file`、`line`。只在这个 skill 自己的目录里读（`toexec-skill` 0.3.0 的 `dir` 模块，和 gld 的 `get_skill` 同一份规则）：普通相对路径、解析后不出目录、路上没有点开头的名字、≤ 1 MiB、UTF-8。一次最多回 64 KiB，在行边界截断，末尾写明下一段的 `line`。加载 skill 时开头多一行列出目录里的其他文件。

**开关**：`[machine_skills]`，每台机器写自己的，不跨机器（和 `[ui]` 一样）。默认 `enabled = true`；`hidden` 按名字藏，只管装好的。**没有跨机器传的东西**：`ResolveReport` / `RunRequest` / `StartRequest` / session 记录一个字段都没加，所以两台机器版本不一致时不会因为这个互相拒绝。

## 会不会丢信息、数据量大不大

**Agent 上的 skills 根本不过 Runtime 链路**：它们在 Agent 上读、由 Agent 上的进程交给同一台机器上的 Claude / Codex。过 ssh 的只有 Runtime 上的那些，而且是按需：目录一次（`tools/list`），正文和附件每次调用一份。

在做这个阶段的这台开发机上实测（真实 HOME，只读）：

| 环节 | 上限 | 这台机器 | 超了会怎样 |
| --- | --- | --- | --- |
| 目录（工具说明） | 2048 个 UTF-16 码元（Claude Code 只留这么多，Codex 不截，按最严的算） | 装了 97 个 skill：**改之前只剩一句"装了 97 个"**，名字都放不下；改之后 84 个名字进了说明（2036 码元） | 先去描述、再只列名字；名字也放不全时列放得下的那些，再写总数。**项目的永远排前面** |
| 全表（不带名字调一次） | 每条描述 1536 字符 | 33.9 KB，97 条 | 一次调用拿全，没截过 |
| 条数 | 100 | 97 | 超了先丢装好的，全表末尾写明 |
| 正文 | 64 KiB / 次，文件 ≤ 1 MiB | 最大的两个 SKILL.md 85 KB、87 KB 会被分成两段 | 在行边界截断，写明接着读的 `line` |
| 附件 | 64 KiB / 次，文件 ≤ 1 MiB，最多列 100 个、4 层 | — | 同上；超过 1 MiB 的不读，报错说大小 |
| 重复 | — | 30 个是两个目录里逐字节相同的拷贝 | 静默算一个（改之前 30 条都进了"没收进来"） |

**会丢、而且是故意丢的**：点开头的文件（`.env` 这类，多半是脚本的密钥）；链接到 skill 目录外面的文件；不是 UTF-8 的文件——Runtime 上的会报出绝对路径，让模型用 `exec_command` 就地用；**Agent 上的二进制附件没有搬运通道**（模板、图片），只能以文字交出；skill 的 `allowed-tools` 等 frontmatter 和 `` !`命令` `` 注入照 P36 不生效、不执行。

## 不要了怎么删

| 仓库 | 删什么 |
| --- | --- |
| ccnm | `mcp/machine_skills.rs`、`mcp/agent_skills.rs`；`skills::Scope` 的 `machine` 一半和 `Origin::Installed` 的分支；`config.rs` 的 `MachineSkills`；`session::create` 的最后一个参数和 `agent-skills.json`；`provider/claude/policy.rs` 里第二个 server 和那条 allow；`provider/codex/mod.rs` 那段 `-c`（`skills.include_instructions=false` 建议留着）；`main.rs` 的 `AgentSkills` 子命令 |
| toexec | `toexec-skill` 的 `dir` 模块（gld 的 `get_skill` 也在用，删之前把它挪回 gld） |

## 验证

- Rust：`cargo test --workspace` 914 passed / 0 failed（P46 时 898，新增 16 条：skills 10、machine_skills 2、agent_skills 2、session 1、codex 1）；fmt、clippy `-D warnings` 通过。
- 中立 MCP 客户端对真实二进制：`tests/test_remote_workspace_mcp.py` 38 条（新增 2 条：Runtime 上装好的 skills）、新文件 `tests/test_agent_skills.py` 3 条（Agent 端服务）；`python3 -m unittest discover -s tests` 199 passed。
- `tools/list` 仍在 16 KiB 预算内：新增 `file` / `line` 的说明让它超了 328 字节，把 `load_skill` 固定说明里逐个列目录的那段删掉换回来（加载结果里本来就给路径）。
- 协议 fixture 手工改（`tools-list-*.json` 的说明和两个参数名），没有重录；`check_protocol` 通过。
- 真实客户端：见上面第四行，toexec `evidence/v3-parity/machine-skills/run_ccnm_agent.py`。

**没验的**：真实模型会不会主动用装好的 skill（要额度）；Codex 不写 `model`（Code Mode）时经 JS 调 `ccnm_agent` 这条路；Linux；执行账号是 `ccrun` 这类空 HOME 时一切照旧，这一点只有推理没有真机。
