# 普通项目样例（P57.3）

两个零依赖的小项目，给“读 → 改 → 测”闭环当靶子：`rust-mini`（Cargo）和 `ts-mini`（Node 自带的类型剥离和测试运行器，不装任何包）。它们不是 ccnm 的测试，也不是真实用户项目；P62 若没有获批的真实项目，可以把其中一个复制成 Runtime 上的测试 workspace，但那仍不算生产项目交付证据。

## 怎么用

先复制出去再 `git init`，别在 ccnm 仓库里原地改——改动要能用 `git reset --hard` 回滚，而仓库里的这份是基线。

| 步骤 | rust-mini | ts-mini | 预期 |
| --- | --- | --- | --- |
| 基线测试 | `cargo test --offline --locked` | `node --test test/duration.test.ts` | 退出 0 |
| 加入失败测试 | `git apply task/01-hours-test.patch` 后再测 | 同左 | 退出非 0，失败的是 hours 那一条 |
| 参考修复 | `git apply task/02-hours-fix.patch` 后再测 | 同左 | 退出 0 |
| 回滚 | `git reset --hard && git clean -fdx` | 同左 | 工作树回到基线，测试退出 0 |
| 大输出 | `cargo run --quiet --offline --bin bigout -- 3` | `node src/bigout.ts 3` | stdout 3 MiB，两边逐字节相同 |

给模型的任务（P62 用）：先应用 `01`，然后只说“`2h` 这样的时长解析失败了，让测试通过”，不给 `02`。`02` 只用来证明任务本身可解、离线闭环可复现。

大输出的 stdout 头、正中、尾各有一个标记 `P57-EARLY-MARKER` / `P57-MIDDLE-MARKER` / `P57-LATE-MARKER`，尾部带中文；stderr 单独一行 `P57-STDERR-MARKER`。用它检查结果链路是丢了头还是丢了中间、按字节截断会不会切坏字符、两个流有没有混在一起。

## 前提

- `rust-mini`：任意支持 lockfile v4 的 stable Rust（本机 1.98.0 验过）。`Cargo.toml` 里的空 `[workspace]` 不能删：没有它，在 ccnm 仓库内跑 cargo 会报 “current package believes it's in a workspace when it's not”。
- `ts-mini`：Node ≥ 22.18 才能直接运行 `.ts`（本机 24.9.0 验过）；更老的版本报 `ERR_UNKNOWN_FILE_EXTENSION`，那是 Node 版本问题，不是样例坏了。只用得上 Node 能剥掉的类型写法，写 `enum` 会直接报语法错。

离线闭环脚本：`python3 -B docs/research/probes/p57-sample-projects.py`，它把两个样例复制到临时目录，按上表跑一遍并输出每步的退出码。
