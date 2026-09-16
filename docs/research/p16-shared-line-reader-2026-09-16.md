# P16：有界行读取移到共享 crate（2026-09-16）

## 结论

- `mcp/read.rs` 里的 `next_line` / `trim_cut` / `enum Ending` 移到共享 crate
  `wk-text`（仓库 `workspace-kernel`），ccnm 改成调用它。**行为逐字节不变**，
  既有 24 个 `mcp::read` 测试一条断言都没改。
- 唯一的改动是把写死的 `MAX_SCAN_BYTES` 变成参数 `LineLimits::scan_limit`。
  64 MiB 这个数仍然由 ccnm 定，共享 crate 自己没有策略。
- **两个产品的 `read_file` 没有、也不会统一**。它们的契约不一样（非法 UTF-8
  一个报错一个有损替换、一个一定读到文件尾一个撞预算就停），各自都是对外
  承诺。逐项对照在 workspace-kernel 仓库的 `evidence/v2-k/duplication-audit.md`。
- **ccnm 的 CI 现在会构建失败**，这是本地 `path` 依赖的直接后果，不是 bug。
  见下面「CI 怎么办」。

## 为什么是这个函数

标准库的 `BufRead::lines()` 会把一整行读进内存，一个 2 GB 的单行文件（压缩过的
JS、一行导出的 JSON）因此先分配 2 GB。ccnm 在 P14 为此改了自己的 `read_file`
（[记录](p14-read-long-line-2026-09-16.md)）。gld 的 `search_file_streaming` 也是
`reader.lines()`，所以这个原语有两个真实消费者，不是为了"共享"而共享。

**gld 那边严重程度低得多，别混为一谈**：它的 `search_text` 有 `max_file_bytes`
（默认 2 MiB、最大 64 MiB），超过就整个文件跳过，所以最坏是 64 MiB 进内存，不是
ccnm 当时那种无上限的 2 GB。gld 真正的内存放大器是 `context_lines` 的行克隆，那
是它自己的缺陷，跟共享库无关，没有夹带进这一刀。

## 改了什么

| | 改前 | 改后 |
| --- | --- | --- |
| `next_line` | `mcp/read.rs` 里的私有函数 | `wk_text::next_line` |
| 扫描上限 | 函数里写死 `MAX_SCAN_BYTES` | `LineLimits { keep, scan_limit }` 参数，ccnm 传 `Some(MAX_SCAN_BYTES)` |
| 行终结符 | 私有 `enum Ending` | `wk_text::Terminator`，变体同名 |
| `trim_cut` | `mcp/read.rs` 里的私有函数 | 共享 crate 的实现细节，不再导出 |

`Scan`、`Limits`、`FileChunk`、`render` 和全部对外契约都没动。

## 验证

macOS arm64，Rust 1.98：

| 门禁 | 结果 |
| --- | --- |
| `cargo fmt --all` / `clippy --workspace --all-targets -D warnings` | 通过 |
| `cargo test --workspace` | **719 passed / 0 failed**，与 P14 记录的数字一致 |
| `cargo test -p ccnm-cli --test external_mcp` | 22 passed |
| `python3 -m unittest tests.test_remote_workspace_mcp` | 10 passed（中立 MCP 客户端，不 import ccnm 代码） |
| 共享 crate 自己 | `cargo test` 9 passed（终结符、跨缓冲区 CRLF、切口不留半个字符、扫描上限） |

P14 新增的三条最关键的用例——扫描上限处停下、超长行的切法、跨缓冲区的
CRLF——都在 ccnm 这边原样保留并通过，所以搬家没有改变行为。

## CI 怎么办

用户 2026-09-16 决定共享 crate 先走本地 `path` 依赖，不把 workspace-kernel 推成
远端仓库。直接后果：GitHub Actions 的 runner 只 checkout ccnm 一个仓库，找不到
`../workspace-kernel`，**`cargo` 在解析 manifest 阶段就会失败**，两个 job 都红。

这不是可以绕过去的：optional 依赖也要求 path 存在（cargo 要读它的 manifest 才能
生成 lock），vendor 进来等于又抄了一份。

解除条件，二选一：

1. 把 workspace-kernel 推成远端仓库（私有即可），两边改成 `{ git = ..., tag = ... }`，
   本地开发用 `[patch]` 指回本地路径；
2. 撤回这次链接——`git revert` 本阶段的提交，`next_line` 回到 `mcp/read.rs`。

在此之前，**ccnm 的门禁只能在本地跑**，绿的依据是上面那张表，不是 CI 徽章。
发版前必须先解决其中之一：release 流程也在 Actions 上。

## 没验的

- gld 那一侧还没接（另算一笔，在 gld 仓库记账）。
- 这次没有跑真机、没有换已安装的二进制、没有消耗模型额度。
- CI 没跑过，理由如上。
