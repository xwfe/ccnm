# P18：冻结协议的工具表 fixture 与实现对不上，改由代码兜住（2026-09-16）

平台 macOS（Darwin 25.6.0），代码在 `92830d6` 之后的工作树，二进制是本轮 `cargo build` 的 `target/debug/ccnm`（0.7.0）。没有真机、没有 SSH、没有消耗模型额度。

## 一、发现了什么

用户给 gld 写远端工具白名单时，把 `docs/protocol/fixtures-mcp/` 的两份工具表 fixture 跟实现逐条核对，报告了两类问题。本轮**全部复现**：起一个真实的 `ccnm internal mcp-serve`，`read` 和 `coding` 两种模式各取一次 `tools/list`，跟 fixture 对参数名。

### 1. `apply_patch` 的参数名是错的

fixture 写的是 `changes`：

```json
"apply_patch": { "properties": { "changes": { "type": "array" } },
                 "required": ["changes"] }
```

`crates/ccnm-core/src/mcp/patch.rs:171` 的 `ApplyPatchArgs` 是 `files`，结构体上没有 `#[serde(rename)]` 也没有 `#[serde(alias)]`（第 111 行那个 `rename_all = "snake_case"` 属于 `Op` 枚举）。真实 server 回的就是 `files`。

**照 fixture 实现的 Host 会被直接拒**，而且拒的方式不友好：`tools/call` 里发 `{"changes": [...]}`，参数反序列化失败，每一次写都失败，错误里也不会提示正确的名字。

### 2. 两份 fixture 都是删节版

| 工具 | fixture 原有 | 实际还有 |
| --- | --- | --- |
| `read_file` | path, start_line, max_lines | `end_line`、`max_bytes` |
| `list_files` | path, glob, max_entries | `include_hidden` |
| `search_text` | query, regex, path, max_results | `glob`、`case_sensitive`、`context_lines` |
| `apply_patch` | （名字就是错的） | `dry_run` |
| `exec_command` | cmd, cwd, timeout_ms | `preview_bytes` |

一共 7 个参数漏记，加 1 个名字错误。`workspace_info` 和 `read_output` 两个是对的。

## 二、`check_protocol.py` 为什么拦不住

它把 fixture 对着 `docs/protocol/schema/remote-workspace-mcp-v1.schema.json` 校验，**不对着 Rust 代码校验**。而那份 schema 里 `inputSchema` 写的是：

```json
"inputSchema": { "type": "object" }
```

——任意 object 都通过。fixture 和 schema 都是手写的，两边一起跟二进制漂走，协议检查照样是绿的。这不是脚本的 bug：它开头就写明"不运行 ccnm rpc，也不证明它的行为与契约一致"。缺的是另外半边。

## 三、fixture 的定位：字面 wire 样本

改之前先判定 fixture 到底是"字面报文"还是"形状示意"，因为这决定要补到什么程度。判定为**字面 wire 样本**，三条依据：

1. **七个工具的 `description` 与 `mcp/server.rs` 的 `#[tool(description = ...)]` 逐字节相同**——是复制过来的，不是概括。
2. 其余 fixture 都是实测报文，`$note` 里记的是实测细节（`call-read-file-ok` 那条写的是 Claude Code 2.1.260 的实际显示行为）。
3. **协议正文里没有任何参数表**：第 5 节只有 annotations，第 8 节只顺带提了 `max_lines`/`max_bytes`/`max_entries`/`preview_bytes` 四个上限参数。所以这两份 fixture 是**参数名在文档侧的唯一记载**。

所以参数名和 `required` 必须完整且精确。但每个参数的类型、上下界和说明**仍然写简写**：server 发的是 schemars 从 Rust 类型生成的完整 schema（带 `default: null`、`format: "uint32"`、`type: ["integer","null"]` 这些），把生成细节冻进 fixture，升一次 schemars 或改一句 doc comment 就会假红，而那些都不是契约。

## 四、改了什么

1. **两份 fixture 的参数名与 `required` 补齐**（`tools-list-read.json`、`tools-list-coding.json`）：`changes` → `files`，补上 `dry_run` 和那 6 个漏掉的参数。两份 `$note` 写明哪部分精确、哪部分简写、以谁为准。
2. **加了一道从代码生成的检查**：`crates/ccnm-cli/tests/external_mcp.rs` 的 `tool_arguments_match_the_running_server`。起真实 `internal mcp-serve`，两种模式各取一次 `tools/list`，逐工具比参数名集合和 `required` 集合。
3. **schema 收紧**：新增 `$defs/input_schema`，要求 `type` 和 `properties` 在场，`required` 是字符串数组，不再是"任意 object"。`tool` 和 `read_tool` 都引它。
4. **协议文档第 13 节加一小节「工具的参数名以哪一份为准」**，写明这道检查在哪条命令里，以及 `check_protocol.py` 为什么证明不了它。

### 这道检查只比名字和必填

不比 `description`、类型和数值边界。理由如上：那些是 schemars 从 Rust 类型生成的，比多了会把一次 crate 升级或一句措辞修改变成协议失败。fixture 的 `$note` 和文档里都写了同一个分界，只写一处以那两处为准。

## 五、先红后绿

把 fixture 改回 `changes` 重跑，新检查如期报错：

```
thread 'tool_arguments_match_the_running_server' panicked at crates/ccnm-cli/tests/external_mcp.rs:543:13:
assertion `left == right` failed: tools-list-coding.json: apply_patch publishes different argument names than it takes
  left: ["changes"]
 right: ["dry_run", "files"]
```

改回之后通过。也就是说，这道检查**确实会拦住当初那个缺陷**，不是一条恒真断言。

## 六、门禁

| 命令 | 结果 |
| --- | --- |
| `python3 scripts/check_protocol.py` | 通过，38 + 21 个 fixture |
| `python3 -m unittest tests.test_check_protocol -q` | 24 passed |
| `cargo test -p ccnm-cli --test external_mcp` | 23 passed / 0 failed（原 22，本轮 +1） |
| `python3 -m unittest tests.test_remote_workspace_mcp -q` | 10 passed |
| `python3 scripts/check_plan.py` | 通过 |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 720 passed / 0 failed（P17 时 719，本轮 +1） |
| `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s tests -p 'test_*.py' -q` | 168 passed |
| `git diff --check` | 干净 |

## 七、没覆盖到的

- **只在 macOS 上跑**。改的是 fixture、一个测试和 schema，不涉及平台相关代码，但 Linux 上没复跑。
- **没验真实 Host**。这轮证明的是"fixture 的参数名等于本机 server 发的参数名"，没有让 Claude Code 或 Codex 真去调一次 `apply_patch`。之前 P11/P12 那两轮真机是在名字错着的时候跑的——它们能过，恰恰因为**模型读的是 server 发的 schema，不是 fixture**，所以这个缺陷伤的只有照文档手写实现的人。
- **这道检查只管 `tools/list` 这两份 fixture**。调用结果和启动诊断那 19 份 fixture 仍然只有 schema 层面的检查；它们要对的是报文正文，是另一件事，本轮没做。
- **不比 `description`**，所以 fixture 里的说明文字理论上仍可能和代码漂开。今天是逐字节相同的（本轮核对过），但没有东西拦住明天改一句而忘了 fixture。

## 八、没有改行为

`ccnm.workspace-mcp` 仍是 v1，没升版本。补进 fixture 的 7 个参数**本来就在 wire 上**——这是修正记载，不是加字段，所以不触发冻结声明里"加字段属于加法"那一条，连加法都不是。工具的行为、参数、默认值一个没动。

gld 那边不需要 ccnm 做任何改动：它已经在 `crates/core/src/bridge/tools.rs` 的模块头里写了"参数名以 ccnm 的 `*Args` 结构体为准，不信 fixture"。那条判断在当时是对的，现在 fixture 也对了，但那条注释仍然值得留着——代码永远比文档新。
