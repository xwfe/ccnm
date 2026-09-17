# P37 执行面第一批：搜索模式、整文件覆盖、一行 shell（2026-09-17）

环境：macOS arm64 开发机；ripgrep 15.2.0；Claude Code 2.1.273（未调用，只读打包代码）；Codex 0.154.0（只用了 `codex sandbox`，不碰模型）。没有花模型额度，没上真机。

## 1. 结论

| 新增 | 对齐原生的哪个工具 | 和原生的差别 |
| --- | --- | --- |
| `search_text` 的 `output_mode` / `multiline` / `type` / `include_hidden` | Grep | 默认仍是 `content`；dotfile 默认不搜；`type` 和 `glob` 不能同时给；没有分开的 `-A`/`-B`、分页和 `-o` |
| `apply_patch` 的 op `write` | Write | 只替换已存在的文件，新建仍是 `add`；必须带版本号 |
| `exec_command` 的 `shell` | Bash | 固定 `bash -c`，没有 bash 就报错；工作目录不跨调用保持 |

实现中另外发现并修了一个行为缺陷（第 3 节），还发现一个同源但没修的问题（第 5 节）。

## 2. P37.1 实测

### 2.1 Claude Code 2.1.273 的三个工具

做法沿用 P36.1：`strings` 二进制，在打包 JS 里找工具定义。

**Grep** 的参数说明（原文摘录）：

```text
Output mode: "content" shows matching lines (supports -A/-B/-C context, -n line numbers, head_limit),
"files_with_matches" shows file paths (supports head_limit), "count" shows match counts (supports head_limit).
Defaults to "files_with_matches".
File type to search (rg --type). Common types: js, py, rust, go, java, etc.
Enable multiline mode where . matches newlines and patterns can span lines (rg -U --multiline-dotall). Default: false.
```

它拼 rg 参数的那段函数（去掉无关部分）：起手 `["--hidden"]`，给一组版本库目录各加一条 `--glob !<目录>`，`--max-columns 500`；`multiline` 加 `-U --multiline-dotall`；`files_with_matches` 加 `-l`，`count` 加 `-c -H`；`content` 用 `--json -n`，另外两种用 `--null`；`type` 加 `--type`。

所以：三种输出模式和 `multiline`、`type` 的语义直接照搬；原生**默认搜 dotfile**，ccnm 冻结时是不搜，改默认值要升版本，所以做成 opt-in 的 `include_hidden`。

**Bash**：打包代码里有 `No suitable shell found. Claude CLI requires a Posix shell environment. Please ensure you have a valid shell installed and the SHELL environment variable set.`——它跑的是用户的 bash 或 zsh，没有就不启动。模型习惯写的是 bash 方言。

### 2.2 rg 15.2.0 的实际行为

在草稿目录里造 `src/a.rs`、`b.py`、`.env`、`.cfg/x`、`.github/workflows/ci.yml`、`.git/config`、`src/.hid.rs`，外加 `.gitignore` 排除的 `ignored/x.rs`，逐条跑。

| 问题 | 结果 | 对设计的影响 |
| --- | --- | --- |
| `--json` 能和 `-l` / `-c` 一起用吗 | 不能（rg 的限制） | 三种模式都读 `--json` 流，路径检查只留一处 |
| `--json --max-count 1` | 每个文件只出一条 `match` 事件，读到第一处就停 | 只列文件模式用它 |
| 计数 | `end` 事件里有 `stats.matched_lines`，但提前停止时拿不到最后一个文件的 `end` | 数 `match` 事件，等于匹配行数 |
| `-U --multiline-dotall` 下一个跨行匹配 | 一条 `match` 事件，`lines.text` 是 `"  foo(1,\n     2);\n"`，`line_number` 是首行 | 按行拆开，行号从首行递增 |
| query 里有换行但没开 `-U` | 退出 2，`the literal "\n" is not allowed in a regex`；`-F` 也一样 | 映射成 `CCNM_E_INVALID_ARGS` |
| `-U -F` 带换行的字面量 | 能匹配 | `multiline` 不要求 `regex` |
| 不认识的 `--type=nosuch` | 退出 2，`unrecognized file type: nosuch` | 映射成 `CCNM_E_INVALID_ARGS`；写成 `--type=<名字>` 一个参数，`-x` 这种名字不会被当成开关 |
| `--glob '!.git' --glob '**'`（排除在前） | `.env`、`.cfg/x`、`.github/…`、`.git/config` 全被搜到，`--no-hidden` 不起作用 | **第 3 节的缺陷** |
| `--glob '**' --glob '!.*' --glob '!.git'`（排除在后） | 只剩 `src/a.rs`、`b.py` | 修法 |
| `--type=rust`，不给 glob | `src/.hid.rs` 也被搜到 | 类型也能越过 `--no-hidden`，同样靠末尾的 `!.*` 挡 |
| `--type=py --glob '**/*.rs'` | 返回 `src/a.rs` | 命中 glob 的文件不看类型 → `type` 和 `glob` 同时给直接拒绝 |
| `--glob '**'`，工作区有 `.gitignore: ignored/` | `ignored/x.rs` 被搜到；`--glob '**/*.rs'` 则不会 | **第 5 节没修的问题** |
| 显式 `path: .github` 且末尾有 `!.*` | `.github/workflows/ci.yml` 仍被搜到 | 显式点名的隐藏目录照旧能搜，和改之前一致 |

最后几行的共同原因在 rg 用的 `ignore` crate 里：一条 glob 命中（不论是目录还是文件）就立刻定案，跳过 `.gitignore`、类型和隐藏文件判断；多条 glob 同时命中时最后一条说了算；类型过滤只看文件、排在 `.gitignore` 之后。

## 3. 顺带修的缺陷：glob 把 dotfile 带回搜索

**现象**：`search_text` 的 `glob` 写成 `*`、`**`、`**/*` 时，dotfile 的匹配行会发给模型；`.git/` 被 rg 整个扫一遍，那里的命中由事后路径检查丢掉，没有发出去。工具说明一直写着 "dotfiles and .git are never searched"。

**原因**：argv 里 `--glob !.git` 排在调用方的 glob 前面，调用方的 `**` 后到、胜出；而 glob 优先于 `--no-hidden`。

**修法**：排除规则 `--glob !.*`、`--glob !.git` 放在最后（`include_hidden` 时只放 `!.git`）。单独提交 44d2196；新测试 `a_glob_that_matches_directories_does_not_bring_dotfiles_back` 在旧顺序下失败（`glob *` 时 `.github/ci.yml` 进了结果），新顺序下通过。

这是行为变更：模型用这类 glob 搜到的结果变少。按冻结契约，它把实现拉回到文档写的样子，不是改语义，不升版本。

## 4. 实现与立项时的差异

- **`shell` 用 `bash -c`，不是 v3 方案写的 `sh -c`。** 依据是 2.1 节：原生 Bash 跑的是 bash / zsh，模型写的是 bash 方言；Debian 的 `sh` 是 dash，`[[ ]]`、`set -o pipefail` 在那里意思不同。没有 bash 时报 `CCNM_E_DEPENDENCY` 并说改用 `cmd`，不退回 `sh`。
- **`type` 和 `glob` 同时给时拒绝**，立项时没写这条，依据是 2.2 节。
- **`write` 只替换已存在的文件**，和立项一致；原生 Write 能新建，这里新建仍是 `add`——一个带着版本号来的 `write` 发现文件不见了，更可能是有人刚删了它。
- `exec_command` 的 `required` 从 `["cmd"]` 变成空：JSON Schema 的 `required` 表达不了"两个里恰好一个"。旧调用照样合法。
- 不加新工具，所以 `session::MCP_TOOLS`（Claude 放行清单、Codex `enabled_tools`）没动；Agent 和 Runtime 两边二进制版本不一致时，旧 Runtime 收到 `shell` 会因为缺 `cmd` 报参数错误，不会执行别的东西。

## 5. 没验的、没修的

- **没修**：`glob` 能匹配目录时 rg 搜进 `.gitignore` 排除的目录（`**` 会搜到 `target/`、`node_modules/`）。调整顺序修不了：rg 没有"只作用于文件的 glob"；要么 ccnm 自己按 glob 过滤 rg 的结果（`*.rs` 在 rg 里匹配任意深度的文件名，在 ccnm 的 `Glob` 里只匹配顶层，语义会变），要么拒绝能匹配目录的 glob。两条都改变现有调用的结果，要单独立项。工具说明里 "Files that .gitignore rules out are never searched" 在这种 glob 下不成立，这一点从 P37 之前就是如此。
- **内存**：`multiline` 下一个匹配可以覆盖整份文件，rg 会把它作为一行 JSON 发出来，`stream_lines` 按行读、没有单行上限。这和"文件里有一行几十 MB"是同一个已有问题，P37 只是多了一种触发方式。
- **计数的口径**：`count` 模式不看匹配行的内容，所以 `content` 模式因为含 NUL 或不是 UTF-8 而跳过的行，这里照样计数。
- 真实模型会不会用这些新参数：没验（不花额度）。
- Linux：没跑。`shell` 依赖 bash，Debian / Ubuntu 上 bash 是必装包，但没实测；Alpine 这类默认没有 bash 的系统会报 `CCNM_E_DEPENDENCY`。
- 其他 rg 版本：没测。第 2.2 节的行为都是 15.2.0 的。

## 6. 验证

见 status.json 里 P37 的 evidence（命令、通过数、平台）。
