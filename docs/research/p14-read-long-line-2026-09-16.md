# P14：read_file 读超长单行时内存有界（2026-09-16）

## 结论

`read_file` 以前用 `read_until(b'\n')` 把整行读进内存再检查 64 MiB 扫描上限，内存峰值跟单行长度成正比。现在一行最多保留 `max_bytes + 8` 字节，扫描上限在读的过程中检查，内存峰值不再随行长增长。返回给调用方的文本、截断方式、错误信息都和修前一样。

## 真实二进制对照

同一台 macOS arm64，外部 read 模式的真实 `ccnm internal mcp-serve`，中立客户端调用一次 `read_file`（默认参数），`/usr/bin/time -l` 取进程最大 RSS。文件是一整行 `a` 加一个换行。修前是提交 `16a6991` 的构建（read.rs 与 v0.6.0 相同），修后是本阶段构建，都是 debug 构建。

| 单行长度 | 修前最大 RSS | 修后最大 RSS | 返回 |
| --- | --- | --- | --- |
| 50 MiB | 115.5 MiB | 13.4 MiB | 两边相同：第 1 行截到 max_bytes，`next_start_line=2`，注明 "line 1 is longer than max_bytes and was cut" |
| 200 MiB | 215.3 MiB | 13.2 MiB | 两边相同：`CCNM_E_INVALID_ARGS … reading line 1 would mean walking more than 64 MiB` |

修前 200 MiB 那一行会先整行进内存再报错；换成 2 GB 的一行，就是先分配 2 GB。

## 测试

- `an_endless_line_stops_at_the_scan_limit_instead_of_filling_memory`：一个永不换行的生成式读取器，被读到超过扫描上限加一块（64 KiB）时直接返回 I/O 错误。**修前失败**（`read_until` 一直读下去，撞上这个 I/O 错误，报的是 `cannot read`），修后在上限处停下，报扫描上限。
- `a_long_line_is_cut_the_same_way_it_was_when_it_was_read_whole`：120 万字节的中文单行以 CRLF 结尾，`max_bytes=100` 正好切在一个字中间。断言保留 99 字节、33 个字、不报非法 UTF-8、`line_ending=Crlf`、下一行照常可读、`total_lines=2`。**修前修后都通过**，用来锁住输出不变。
- `a_crlf_split_across_buffers_is_still_crlf`：`\r` 和 `\n` 落在 8 KiB 缓冲区两侧，仍识别为 CRLF。
- 既有 24 个 `mcp::read` 测试断言未改。

门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 通过；`cargo test --workspace` 719 passed / 0 failed；`tests.test_remote_workspace_mcp` 与 Python 全量通过。

## 限制

- RSS 用 debug 构建量，release 数字会更小，比例关系不变。
- 扫描上限本身（64 MiB）和它的报错文字没变；读一个 64 MiB 以内的超长行仍要走完这一行才知道它的换行符，耗时跟行长成正比，只是不再占内存。
