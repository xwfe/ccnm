# P19：工具说明文字也纳入同一道检查（2026-09-16）

平台 macOS（Darwin 25.6.0），代码在 P18 的 `861617d` 之后。没有真机、没有 SSH、没有消耗模型额度。

## 一、为什么 P18 把 description 划在外面是划错了

P18 那道检查只比参数名和 `required`，理由写的是"比多了会在升 schemars 或改一句说明时假红"。

**这个理由对类型和上下界成立，对 `description` 不成立。** 两者的来源不一样：

| 东西 | 谁写的 | 漂了算什么 |
| --- | --- | --- |
| 参数的类型、`minimum`、`format`、`default` | schemars 从 Rust 类型**生成** | 升一次 crate 就可能变，比它是假红 |
| 工具的 `description` | 人手写进 `#[tool(description = ...)]`，fixture 里逐字节复制 | 真漂 |

而且漂的东西不一样重。参数名错了，Host 调用被拒，人能看见。`description` 错了，**模型读到的是 fixture 之外的另一段文本**——但没人会收到错误，因为模型读的本来就是 server 发的那一份；错的只是文档，而文档正是照着它手写 Host 的人唯一能看的东西。

七段说明当前与代码**逐字节相同**（P18 核对过，本轮重新核对仍然相同），所以这一阶段是加一道拦住未来的检查，不是修一个现有缺陷。

## 二、改了什么

### 1. 检查纳入 description，测试改名

`crates/ccnm-cli/tests/external_mcp.rs` 的 `tool_arguments_match_the_running_server` 改名为 **`published_tool_tables_match_the_running_server`**——它比的已经不只是参数。现在比三样：

- 工具名集合；
- 每个工具的 `description`，逐字节；
- 每个工具的参数名集合与 `required` 集合。

**仍然不比每个参数内部**的类型、上下界和说明，理由同上。

### 2. 同步四处引用

`tools-list-read.json` 和 `tools-list-coding.json` 的 `$note`、schema 里 `$defs/input_schema` 的说明、协议文档第 13 节那一小节（标题从"工具的参数名以哪一份为准"改成"工具表以哪一份为准"，并补了一张"比什么、为什么"的表）。

`docs/research/p18-tool-args-drift-2026-09-16.md` 和 `status.json` 里 P18 的 evidence **没有改**：那是那一轮的历史记录，记的是当时的事实。要找那个名字的人看这一份。

### 3. 故意不做自动重录

没有加"跑一次就把 server 输出写回 fixture"的开关。`AGENTS.md` 写着"不为通过测试重录 golden fixture"，而那种开关正好能让下一个人把一次没想清楚的措辞改动一键洗成绿的。改了说明就手动同步 fixture——失败信息把两段文本都打出来，照着贴即可。协议文档里也写了这一条。

## 三、先红后绿

把 `tools-list-read.json` 里 `read_file` 的说明改一个词（`as numbered lines` → `as numbered lines of text`）重跑：

```
thread 'published_tool_tables_match_the_running_server' panicked at crates/ccnm-cli/tests/external_mcp.rs:555:13:
assertion `left == right` failed: tools-list-read.json: read_file publishes a description this server does not serve
  left: "Read a text file from the remote workspace, as numbered lines of text. Paths are relative to the workspace root. ..."
 right: "Read a text file from the remote workspace, as numbered lines. Paths are relative to the workspace root. ..."
```

改回之后通过。失败信息里 `left`（fixture）和 `right`（server）两段都在，同步就是一次复制。

## 四、门禁

| 命令 | 结果 |
| --- | --- |
| `python3 scripts/check_protocol.py` | 通过，38 + 21 个 fixture |
| `python3 -m unittest tests.test_check_protocol -q` | 24 passed |
| `cargo test -p ccnm-cli --test external_mcp` | 23 passed / 0 failed |
| `python3 -m unittest tests.test_remote_workspace_mcp -q` | 10 passed |
| `python3 scripts/check_plan.py` | 通过 |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 720 passed / 0 failed |
| `PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s tests -p 'test_*.py' -q` | 168 passed |
| `git diff --check` | 干净 |

**测试总数与 P18 相同（720、23）**：这一轮是给已有的那个测试加断言，没有新增测试函数。

## 五、annotations 为什么不纳入

它已经被两处证明着，再加一处就是第三份说明：

- `every_tool_publishes_its_annotations` 拿真实 server 的 annotations 对着硬编码期望比（七个工具的 `readOnlyHint`/`destructiveHint`/`openWorldHint`）；
- schema 的 `$defs/read_tool` 把 read 模式的 `readOnlyHint` 钉成 `const: true`、`openWorldHint` 钉成 `const: false`，`check_protocol.py` 会校验。

改 `annotations_for()` 会被第一条拦住，fixture 里写错 read 模式的注解会被第二条拦住。所以缺口不在这里。

## 六、没覆盖到的

- **只在 macOS 上跑**。改的是一个集成测试和几处文档，不涉及平台相关代码。
- **仍然不比每个参数内部的类型和上下界**，理由见第一节，这是有意的边界，不是遗漏。
- **调用结果和启动诊断那 19 份 fixture 仍只有 schema 层面的检查**。它们要对的是报文正文，是另一件事，要做得另立阶段。
- **没有行为变化**。`ccnm.workspace-mcp` 仍是 v1；工具的说明文字一个字没改，只是现在有东西盯着它了。
