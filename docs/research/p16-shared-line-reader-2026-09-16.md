# P16：有界行读取移到共享 crate（2026-09-16）

## 结论

- `mcp/read.rs` 里的 `next_line` / `trim_cut` / `enum Ending` 移到共享 crate
  `toexec-text`（仓库 `toexec`），ccnm 改成调用它。**行为逐字节不变**，
  既有 24 个 `mcp::read` 测试一条断言都没改。
- 唯一的改动是把写死的 `MAX_SCAN_BYTES` 变成参数 `LineLimits::scan_limit`。
  64 MiB 这个数仍然由 ccnm 定，共享 crate 自己没有策略。
- **两个产品的 `read_file` 没有、也不会统一**。它们的契约不一样（非法 UTF-8
  一个报错一个有损替换、一个一定读到文件尾一个撞预算就停），各自都是对外
  承诺。逐项对照在 toexec 仓库的 `evidence/v2-k/duplication-audit.md`。
- 共享 crate 按 tag 从 `https://github.com/xwfe/toexec.git` 拉（公开仓库，与 ccnm、
  gld 一致）。**在一个旁边没有 toexec 的目录里构建通过**——那正是 CI
  runner 的处境。中途一度用过本地 `path` 依赖，两边 CI 都因此构建不了，见下面
  「依赖方式：从 path 到 git tag」。

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
| `next_line` | `mcp/read.rs` 里的私有函数 | `toexec_text::next_line` |
| 扫描上限 | 函数里写死 `MAX_SCAN_BYTES` | `LineLimits { keep, scan_limit }` 参数，ccnm 传 `Some(MAX_SCAN_BYTES)` |
| 行终结符 | 私有 `enum Ending` | `toexec_text::Terminator`，变体同名 |
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

## 依赖方式：从 path 到 git tag

本阶段中途先用的是本地 `path` 依赖（`../toexec/crates/toexec-text`），当时
toexec 还没有 remote。**那让两边 CI 都构建不了**，而且是在解析 manifest
的阶段就死，实测：

```text
error: failed to load manifest for workspace member `.../crates/ccnm-cli`
Caused by: failed to load manifest for dependency `ccnm-core`
Caused by: failed to read `.../toexec/crates/toexec-text/Cargo.toml`
Caused by: No such file or directory (os error 2)
```

连依赖解析都到不了，所以任何 `cargo` 子命令都一样失败。也绕不过去：optional
依赖同样要求 path 存在（cargo 要读它的 manifest 才能生成 lock），vendor 进来等于
又抄了一份。

同日用户决定把 toexec 推上去，改成按 tag 的 git 依赖：

```toml
toexec-text = { git = "https://github.com/xwfe/toexec.git", tag = "toexec-text-v0.1.0" }
```

三个选择，理由都写在 `Cargo.toml` 那一行旁边：

- **按 tag，不跟 `main`**：共享库改了不会在某次 `cargo update` 之后突然改变 ccnm
  的行为。升级是显式的一步——那边发新 tag，这边改这一行。
- **https 不是 ssh**：`toexec` 是公开仓库（和 ccnm、gld 一样），匿名就能读，本地
  和 CI 都不必配凭据。`github.com-xwfe` 那种 SSH 别名只存在于本机的 `~/.ssh/config`，
  写进 `Cargo.toml` 的话 runner 上永远解析不了。
- **本地开发那边的源码时临时改成 path，不提交**——提交了 CI 就又拉不到了。

**验证方式就是 CI runner 的处境**：把 ccnm clone 到一个旁边没有 toexec
的目录，`cargo check --workspace` 通过，cargo 自己从 GitHub 把 `toexec-text v0.1.0
(tag=toexec-text-v0.1.0#25ef24a8)` 拉了下来。gld 同样验过。

## 没验的

- gld 那一侧还没接（另算一笔，在 gld 仓库记账）。
- 这次没有跑真机、没有换已安装的二进制、没有消耗模型额度。
- **GitHub Actions 上没有真跑过一次**。验的是同一件事的本地等价物（旁边没有
  toexec 的目录里 `cargo check` 通过），推上去之前不知道 runner 上还有
  没有别的问题。
