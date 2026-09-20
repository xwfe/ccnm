# 真实模型第一次用上 P36–P44 的工具（2026-09-20）

P36 到 P44 加的东西，支持矩阵里每一行都挂着同一句"没验的：真实模型会不会用（没花模型额度）"。这一轮花了 **$0.57** 把其中几条换成证据。

**结论先说**：`read_notebook`、`view_image`、`load_skill`、`exec_command` 的 `shell` 都被真实模型用了，图真的进了模型的眼睛，P44 的参数校验没有咬到真实 Claude Code。**`run_in_background` 没拿到证据**——不是没用，是看不出来，原因在第 4 节。

## 1. 怎么跑的

两台都是 `0.8.0` 的本机构建（`cd9e8e0`）：Runtime 是本机 macOS，Agent 是 fodelf（macOS，Claude Code 2.1.272，claude.ai max 登录）。

```bash
ccnm run ccnm --print '帮我做三件事，都在这个项目里：

1. tests/fixtures/notebook/analysis.ipynb 的第一个 cell 在算什么？
2. tests/fixtures 底下有一张 PNG，它画的是什么？
3. 跑一遍这个项目的提交门禁。其中有一步要跑一两分钟，别让自己干等在那儿。

三件事做完，每件用一两句话告诉我结果。'
```

**为什么这么问。** 要验的是"模型会不会想到用"，不是"能不能调得通"，所以一个工具名都没提。三个问题各自只有一条路走得通：notebook 的 cell 结构、一张图的内容、一套项目专有的门禁步骤。

**两个临时夹具，跑完就删了**（这个仓库本来没有图片，也没有 SKILL.md）：

- `tests/fixtures/realmachine-probe/stripes.png`——48×48，上红中绿下蓝三条横杠，126 字节，用 `zlib` + `struct` 手写，不依赖 PIL。
- `.claude/skills/run-gates/SKILL.md`——写明门禁是 `cargo fmt --all --check` → `python3 scripts/check_plan.py` → `cargo test --workspace` 三步，并说第三步要跑一两分钟、建议放后台。`.claude/skills` 是 `SKILL_DIRS` 的第一个位置。

**模型是哪个没记下来。** ccnm 的 legacy Claude 配置没有 `--model` 这个口子，fodelf 的 `~/.claude/settings.json` 也没有显式 `model` 键（只有 `CLAUDE_CODE_SUBAGENT_MODEL: opus`），所以跑的是 Claude Code 2.1.272 当时的默认主模型，具体是哪个没有落在任何记录里。**下一轮要把模型名记下来**，否则换了默认模型这份证据就说不清适用范围。

## 2. 结果

```
会话      3ee941a6-bc57-46e8-9547-2879147908f2
起于      ccnm 0.8.0 as fodelf, pid 84473
claude    exited 0 in 146.5 s
结果      20 turns in 144.3 s (api 69.8 s)
          tokens in 26 out 4839 cache-write 32901 cache-read 235816
          $0.57; 0 permission denials
```

## 3. 哪些工具被用了，凭什么这么说

证据的地基是 ccnm 给这次会话生成的 `settings.json`：`allow` 列的是 11 个 `mcp__ccnm__*` 工具，`deny` 列的是 Claude Code 自带的 `Read`、`Edit`、`Write`、`Grep`、`Glob`、`Bash`。**模型没有第二条路可走**，所以它答上来了就说明它用了对应的 ccnm 工具。

| 工具 | 模型说了什么 | 为什么这算证据 |
| --- | --- | --- |
| `read_notebook` | "第一个 cell 是 markdown，不在算东西；真正算的是第二个 cell，用 pandas 读 `sales.csv`，存的输出是 `rows: 3` / `columns: 2`" | `Read` 被 deny。而且它分得清 cell 类型、读得到**存储的输出**——`read_file` 拿到的是一坨 JSON 文本，答不出这个形状 |
| `view_image` | "三道横条：上红、中绿、下蓝，126 字节" | 和夹具逐条对得上。**这是第一次证明图片真的到了模型面前**，此前支持矩阵写的是"Claude Code 实际把图发给模型（没登录，看不到请求）" |
| `load_skill` | 跑的门禁是 fmt → `check_plan.py` → `cargo test --workspace` | 三步的内容和顺序跟 SKILL.md 一字不差。`scripts/check_plan.py` 是这个项目专有的，不读 skill 猜不出来 |
| `exec_command` 的 `shell` | —— | 进程快照抓到 `bash -c export PATH=...; cargo test --workspace 2>&1; echo "=== test exit: $? ==="`。带 `;` 和重定向，`cmd` 那种 argv 数组拼不出来 |

顺带看到的两件事，都和源码说的一致：

- 写锁文件内容是 `held 3ee941a6-... ccnm pid 69167`，**P43 加的 pid 字段在真实会话里如实写着**。
- 那条命令的 `pgid` 等于它自己的 pid（`70719 69167 70719`），即 ccnm 给每条命令单独建了进程组——`stop_command` 能整组杀掉的前提。

**P44 的收紧没有咬到真实 Claude Code。** 20 轮工具调用，`0 permission denials`，没有一次 unknown field 报错。这条此前在支持矩阵里标着"没额度没登录，是推理不是证据"。一次会话覆盖不了所有情况，但至少这个 Host 没往这些工具里塞它们没声明的字段。

## 4. `run_in_background` 为什么没有结论

**不是"模型没用"，是"看不出来用没用"。**

`--print` 模式下 Claude Code 没在 `~/.claude/projects/<编码过的 cwd>/` 留 JSONL，那个目录里只有一个空的 `memory/`，所以逐次工具调用的参数拿不到。剩下的只有进程快照，而前台命令和后台命令在进程树上长得一模一样：都是 `mcp-serve` 的子进程，都自成一个进程组。

而且时长也区分不了：`cargo test --workspace` 跑了 81.3 秒，**前台默认上限是 120 秒，前台跑得完**。

所以尽管 prompt 写了"别让自己干等在那儿"、SKILL.md 也写了"放到后台去跑，然后隔一会儿读一次输出"，仍然不能断言它照做了。

**下一轮要怎么拿到这个证据**：给 `mcp-serve` 开 `CCNM_LOG=debug`，工具调用会落进日志；或者换交互模式，Claude Code 会留会话记录。

## 5. 顺手撞出来的真问题：`cargo` 不在 PATH

模型第一次跑 `cargo test` 拿到的是 `bash: cargo: command not found`，exit 127。

**原因**：Runtime 这台机器的 PATH 里挂着 `~/.cargo/bin`，**而那个目录根本不存在**；rustup 装在 `/opt/homebrew/bin`，工具链实体在 `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin`。项目没有 `rust-toolchain.toml`。

模型自己把工具链目录前置到 PATH 绕过去了，并指出了真正危险的地方：**这种失败快得像秒过，容易被当成"通过"，其实一行测试都没跑。**

修法是在 Runtime 上跑一次 `rustup default stable`，让它把 shim 补回 `~/.cargo/bin`。**本轮没动这台机器的环境**。

## 6. 这一轮没验的

- `run_in_background` / `read_output` / `stop_command`（见第 4 节）。
- 按 Esc 实际发出的取消通知——`--print` 模式里没有 Esc。
- Codex 当 Host。
- Linux Runtime：P36–P44 全部只在 macOS arm64 上跑过。
- `search_text` 的新参数（`output_mode` / `multiline` / `type`）：这次任务没有引出它们。
- 一次会话、一个模型、一个任务。**不要把这份记录读成"真机验收通过"**，它只是把几条"推理"换成了"这一次确实如此"。
