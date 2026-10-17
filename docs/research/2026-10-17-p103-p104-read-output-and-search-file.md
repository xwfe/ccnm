# P103–P104：`read_output` 误报"limit 太小"、`search_text` 收单个文件（2026-10-17）

## 1. 由来

2026-10-17 统计 fodelf（AI 那台）上 10-08 起全部 ccnm MCP 日志里模型撞到的报错（[P100–P102 记录第 2.4 节](2026-10-17-p100-p102-session-entry-and-handoff.md)），排第一和排第四的两条和会话、网络无关，是工具本身的问题：

| 次数 | 工具 | 报错 | 性质 |
| --- | --- | --- | --- |
| 51 | `search_text` | `<path> is not a directory; search_text searches a tree` | 模型照原生 Grep 传了一个文件；冻结协议的加法（P104） |
| 10 | `read_output` | `limit N is too small to hold one character at this offset`，N 是 4000–7000 | 缺陷（P103） |

## 2. P103：`read_output`

### 2.1 原因

`crates/ccnm-core/src/mcp/output.rs` 的旧 `boundary_before` 用 `std::str::from_utf8(buf).valid_up_to()` 找页尾。这个值是**第一个**不合法字节的位置，不是"末尾没写完的那个字符"的位置。两种情况下它是 0，整页被当成 0 字节，报"limit 太小"：

1. **offset 落在多字节字符中间。** 模型读尾部时常拿 `total_bytes` 减一个数当 offset，输出里有中文、`✓`、`…` 时，三字节字符占多数，落进中间的概率约三分之二。第一个字节是续字节，`valid_up_to()` 是 0。
2. **输出中间有一个不是 UTF-8 的字节。** 这一页在它前面结束，下一页从它开始，`valid_up_to()` 又是 0——从这里起每一页都报错，按 `next_offset` 永远读不过去。

只有"读到流的末尾、命令也跑完了"那一页不经过这段判断（直接整页交出、坏字节换 U+FFFD），所以报错总出现在长输出的中间页、或者后台命令还在跑的时候。

### 2.2 修法

- **页头**：从 offset 往前多读最多 3 字节。offset 处是续字节、前面 3 字节内有一个合法的前导字节、而且它的字符还没完时，往后挪到下一个字符开头（最多 3 字节）；页里多一行 `[offset N is inside a character; this page starts at offset M, where the next one begins]`，`offset` 字段报 M。前面找不到前导字节的续字节是坏字节，不跳，照常换成 U+FFFD——跳过它等于悄悄丢一个字节。
- **页尾**：只切掉最后一个"开了头没写完"的字符（`from_utf8` 对那段返回 `valid_up_to() == 0 && error_len() == None`）。中间的坏字节留在页里，换成 U+FFFD，带原有那行 `this stream is not valid UTF-8; invalid bytes were replaced`。
- **页的大小**：往后多读 3 字节，挪了起点之后这一页仍能有 `limit` 字节。所以"limit 太小"只在 limit 真装不下一个字符（1–3 字节对一个三四字节的字符）时报。
- 不变的：offset 是字节偏移、稳定；上限 32 KiB、默认 16 KiB；错误码；读到末尾且命令还在跑时，最后一个没写完的字符留到下次（空页说"到目前为止"，不报错）。

### 2.3 验证

- `output.rs` 新增五条单元测试、扩了一条，**在旧代码上六条全红**（五条报 `too small`，一条 offset 不对）：offset 4000 落在"中"中间、limit 4000（页从 4002 开始、3999 字节）；一段 1、2、3、4 字节字符混排的流，每个 offset 都从下一个字符边界读、读出的正好是从那里起的原文；中间夹一个 0xff 的 2.4 KB 输出按 `next_offset` 用 64、1000、1024 三种 limit 读完，拼起来等于 `from_utf8_lossy` 的全文；`a\x80bbb…` 从 1 读不跳那个孤立续字节；后台命令还在跑、最后一个字符只写了两字节时，那两字节留到下次、再读是空页而不是报错；limit 1、2 对"中"仍报太小、limit 3 从字符中间读得出。原有分页测试没改，照过。
- 中立 MCP 客户端（`tests/test_remote_workspace_mcp.py`，不 import ccnm）加一条：`exec_command` 写出 12001 字节（2000 个"中"、一个 0xff、再 2000 个"中"），从 offset 4000 读、再按 `next_offset` 每页 4000 读完。**拿旧 `output.rs` 编出的二进制跑，它报的就是 fodelf 日志里那句** `CCNM_E_INVALID_ARGS: limit 4000 is too small to hold one character at this offset`；新二进制上过。

## 3. P104：`search_text` 的 `path` 是一个文件

### 3.1 rg 15.2.0 实测：点名的文件不受那几条排除规则管

夹具 [`probes/p104-rg-explicit-file.py`](probes/p104-rg-explicit-file.py)，本机 macOS arm64、ripgrep 15.2.0。每一行的参数都是 `search.rs` 的 `Plan::command` 拼出来的那些（`--json --no-config --no-follow --no-hidden --case-sensitive --fixed-strings --glob '!.*' --glob '!.git'`），只换点名的路径和额外参数：

| 点名的 | 额外参数 | rg 的结果 |
| --- | --- | --- |
| 普通文件 `src/main.rs` | — | 搜了 |
| `.gitignore` 排除的文件 `src/gen.rs` | — | **搜了** |
| `.gitignore` 排除的目录里的文件 `ignored/x.rs` | — | **搜了** |
| dotfile `.hidden` | — | **搜了**（`--no-hidden`、`!.*` 都不管） |
| 点开头目录里的文件 `.github/ci.yml` | — | **搜了** |
| `.git/config` | — | **搜了**（`!.git` 不管） |
| 指向文件的符号链接 `link.rs` | — | **搜了**，跟到了目标（`--no-follow` 不管） |
| 指向 `.git/config` 的符号链接 `gitlink` | — | **搜了**，报的路径是 `gitlink` |
| `src/main.rs` | `--type=py` | **搜了**（类型不管） |
| `src/main.rs` | `--type-add=ccnmglob:*.py --type=ccnmglob`（ccnm 给 glob 缩范围的写法） | **搜了** |
| `src/main.rs` | `--glob '!*.rs'` | **搜了**（排除 glob 不管） |
| 二进制 `blob.bin`（`needle` + 64 个 NUL + `needle secret`） | — | **两处都发了**：`needle`、`needle secret`，`end` 里 `binary_offset=6`。显式文件按"NUL 当换行"整份搜，`--json` 不压 NUL 之后的匹配 |
| 目录 `ignored`（`.gitignore` 排除的） | — | 搜了（今天就这样） |
| 目录 `.git` | — | rg 搜进去报 `.git/config`；ccnm 的事后检查按路径丢掉，模型看到"没匹配"加一行"rg 给了 workspace 外的路径" |
| 目录 `gitdir`（指向 `.git` 的符号链接） | — | **rg 搜进去报 `gitdir/config`，ccnm 的事后检查只看路径里有没有 `.git` 这一段，拦不住：`.git/config` 的内容发给了模型。这是现有缺口** |
| 目录 `srclink`（指向 `src`） | — | 搜了，跟进去 |
| 对照：从根遍历 `./` | — | 只有 `src/main.rs`：被忽略的、dotfile、`.git`、符号链接、二进制都不搜 |

一句话：**rg 对命令行上点名的路径一律照搜**，ccnm 加在 rg 上的约束只管遍历时遇到的东西。

### 3.2 定下的规则

原则（2026-10-17 这次任务给的）：和 `read_file` 能读到的范围一致，`.git` 里的永远不搜。

| `path` 是 | 做法 | 依据 |
| --- | --- | --- |
| 普通文件 | 只搜它 | 原生 Grep 收文件 |
| 点名的 dotfile、被 `.gitignore` 排除的文件 | 搜 | `read_file` 读得到；rg 本来就照搜 |
| workspace 里的符号链接 | 跟，搜目标；路径策略和 `read_file` 是同一个函数，指出 workspace 的照旧 `CCNM_E_POLICY` | `read_file` 也跟 |
| `.git` 里的文件或目录，包括经符号链接解析到 `.git` 里的 | `CCNM_E_POLICY` | 工具说明写着 `.git never`；解析之后再查，才拦得住 `gitdir`、`gitlink` |
| 不是普通文件也不是目录（fifo 等） | `CCNM_E_INVALID_ARGS` | 和 `read_file` 同一句；rg 打开 fifo 会一直等 |
| 头 8 KiB 有 NUL | `CCNM_E_INVALID_ARGS`，说是二进制 | 和 `read_file` 同一条线；不然 rg 会把 NUL 之后的内容当文本发回来 |
| 给了 `glob` 或 `type` | 不用，结果里写一行说明 | rg 对点名的文件本来就不管它们（原生 Grep 同样不管）；ccnm 自己按 glob 过滤会让同一个调用在原生和 ccnm 上结果不同 |

`path` 是目录时只多了 `.git` 那一条拒绝，其余不变。
