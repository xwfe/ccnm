# 测试建的临时目录跑完就删（P35，2026-09-17）

验收见 ROADMAP 的 P35。起因记在 [P34 记录](p34-journal-lock-release-2026-09-17.md)第 4 节和 `status.json` 的 observed_gaps：测试的 fixture 目录没有 teardown。

**一句话结论**：一次全量 `cargo test --workspace` 原来在 `$TMPDIR` 和 `/tmp` 留下 421 个新目录，现在留 0 个。做法是一个只在测试里用的守卫类型 `TestDir`——离开作用域就删目录。只动了测试代码，没改任何断言和产品代码。

## 1. 问题

fixture 目录的名字里带 pid（`ccnm-read-<pid>-<测试名>`）或会话 id，这是为了让同时跑的几个测试进程互不踩。副作用是：**上一次运行留下的目录，下一次运行永远不会复用或覆盖**，每跑一次测试二进制就多一批。

这个仓库排查偶发失败的常规做法，是直接循环跑测试二进制几十上百次（P34 一轮就跑了几百次）。本机 `$TMPDIR` 里攒到过 18 万个 `ccnm-*` 目录、把盘写满；P34 清过一次（90,383 个、1.03 GiB），但不改代码的话只是重新开始攒。

## 2. 基线（P35.1）

量法：跑之前记下 `$TMPDIR` 和 `/tmp` 下所有 `ccnm-*`、`cp3-*` 条目（含 `/tmp/ccnm-t`、`/tmp/ccnm-lt`、`/tmp/ccnm-cli-st` 三个共用父目录的下一层），跑完再数一次，取新增的。

改之前（提交 59374e3），macOS 26.6.2 arm64，`cargo test --workspace` 809 passed / 0 failed：**新增 421 个**（`$TMPDIR` 419，`/tmp` 2）。按名字前缀分组：

```text
ccnm-rpc 72 | ccnm-patch 58 | ccnm-work 26 | ccnm-read 26 | ccnm-doctor 22 | ccnm-path 19
ccnm-safety 18 | ccnm-search 18 | ccnm-e2e 17 | ccnm-rpc-it 14 | ccnm-runtime 14 | ccnm-exec 13
ccnm-list 12 | ccnm-output 11 | ccnm-mcp 11 | ccnm-native-policy 10 | ccnm-cli 10 | ccnm-retention 7
ccnm-rpcstore 6 | ccnm-cfgedit 6 | ccnm-session 5 | ccnm-ctx 5 | ccnm-open 4 | ccnm-launcher 4
ccnm-ctl-sess 3 | 其余 10 个前缀各 1（含 /tmp 下的 ccnm-cli-home、ccnm-ctl）
```

没出现在这张表里的 fixture（`write_guard`、`exec_serve`、`external_mcp`、`provider/codex` 等）本来就会删：要么 `Fixture` 有 `impl Drop`，要么测试末尾自己 `remove_dir_all`。

## 3. 为什么是 Drop 守卫，不是别的

- **进程退出钩子用不了。** workspace 的 lint 是 `unsafe_code = "forbid"`，也没有 libc 依赖；`atexit` 要 `unsafe extern`。
- **用 cargo 的 `runner` 给每次运行包一层私有 `TMPDIR` 不行。** 直接跑 `target/debug/deps/ccnm_core-…` 会绕过它，而那正是攒得最快的用法。
- **把 `TMPDIR` 指到 `target/tmp` 不行。** 只是换个地方攒；而且本机上，仓库目录树里带 `#!/usr/bin/env python3` 的假脚本会撞上 mise 的 shim（P33 记过）。

所以只剩 Drop。

## 4. 改了什么（P35.2、P35.3）

**守卫**在新的 workspace 成员 `crates/ccnm-testdir`（`publish = false`，只作 dev-dependency，不进产品二进制）。单独成 crate 是因为三种测试——`ccnm-core` 的单元测试、`ccnm-core/tests`、`ccnm-cli/tests`——要用同一份定义，另外两条路是抄三份，或者在产品库里开一个公开模块。

- `TestDir::adopt(path)` 接管一个调用方自己算好的路径，不建目录；`.also(path)` 再带一个（socket 目录在 `/tmp`、其余在 `$TMPDIR` 的 fixture 用）。
- drop 时 `remove_dir_all`，不存在不算错。**所在线程正在 panic（测试失败）时不删**，把路径打到 stderr——失败现场通常是最快的排查线索。
- 能当 `&Path` 用（`Deref`、`AsRef<Path>`、`AsRef<OsStr>`），所以把 fixture 函数的返回类型从 `PathBuf` 换成 `TestDir` 之后，绝大多数调用点不用动。
- **路径怎么起名、放哪、要不要 canonicalize 仍由各 fixture 决定**，测试看到的路径一个字节不变。这一点不能让守卫接管：ssh ControlPath 和 Unix socket 有 103 字节上限（所以那几处在 `/tmp` 下），Codex 不肯在临时目录里建沙箱辅助程序（所以 `exec_sandbox` 的夹具在 `target/tmp`）。

**逐个文件**（32 个文件，+425 / −143 行）。调用点要动的只有四类：

| 情况 | 改法 | 不改会怎样 |
| --- | --- | --- |
| `root.clone()` 要一个 `PathBuf` | 换成 `.to_path_buf()` | 编译不过（守卫故意不能 clone） |
| `Store::open(&temp("x"))`、`workspace("x").join("ws")` 这类**临时值** | 先绑到变量上 | 编译能过，但语句一结束目录就被删了，测试在一个不存在的目录上跑 |
| fixture 返回的不是要删的那一层（`path`、`mcp_read_file` 返回工作区根，要删的是上一层） | `adopt(root).also(上一层)` | 根外面的 `secret.txt`、`config.toml` 留下 |
| 目录要活过建它的那次调用 | 守卫交给活得够久的人 | 见下面两条 |

两处"活得够久"：`mcp/server.rs` 的 `initialized_client` 起一个后台服务端任务，守卫跟任务句柄一起交回调用方；`ccnm-cli/tests/rpc.rs` 的沙盒要被 `talk_reusing` 再次打开（装成同一个会话库的第二个客户端），所以守卫由测试函数持有（每个测试开头一行 `let _tidy = tidy("名字");`），不放在 `talk()` 里。

**三个"每进程一个、所有测试共用"的目录改成了每测试一个**——共用的目录没有哪个测试的结束可以删它：

- `controller.rs` 的 `socket()`：`/tmp/ccnm-ctl-<pid>/<测试>.sock` → `/tmp/ccnm-ctl-<pid>-<测试>/c.sock`，路径长度多 2 个字符。
- `cli.rs` 和 `runtime_open.rs` 的 `ccnm()`：给子进程的假 `HOME`。`ccnm()` 返回 `Command`，带不了守卫，而且一个测试里要调很多次，所以用 `thread_local!`：libtest 每个测试一个线程，线程结束时 TLS 析构把目录删掉。`--test-threads=1` 下单独验过（那种模式下 libtest 仍然每个测试一个线程），残留 0。

**没交给守卫的**（全量运行后这些前缀的新增残留本来就是 0）：

- 已有 `impl Drop` 的 `Fixture`：`ccnm-cli/tests` 的 `exec_sandbox`、`exec_serve`、`exec_transport`、`external_mcp`、`instance_execution`、`provider_safety`、`write_guard`，`ccnm-core/tests` 的三个文件，`native/serve/tests.rs` 的 `Scratch`，`safety/provider_tests.rs`。
- 测试末尾自己 `remove_dir_all` 的：`launchagent.rs`、`process.rs`、`mcp/sandbox.rs`、`mcp/write_guard.rs`、`provider/codex/tests.rs`，以及 `controller.rs`（`ccnm-spawn`、`ccnm-pgid`、registry）、`safety.rs`（`ccnm-accepted`）、`external_mcp.rs`（`ccnm-old`）里的几个内联目录。成功时删、失败时留，和守卫的行为一样，不动。
- `work.rs`、`launcher.rs` 里直接写在 `/tmp` 下的 `ccnm-*-<pid>.sock`：真的会 bind，但绑它的是产品代码的 `controller::Listener`，它的 Drop 会删 socket 文件。
- 只拼路径、从不建东西的：`launchagent.rs` 的 `ccnm-not-there.*`，`work.rs` 的超长 ControlPath，`native/serve/tests.rs` 里 `Policy::new(temp_dir(), …)`，`process.rs` 的 `pwd` 测试，`launcher.rs` 的 `control()`（FakeRunner 不会真的起 ssh，那个目录从没被建出来）。

## 5. 验证（P35.4）

同一台机器、同一种量法：

| | 测试结果 | 新增残留 |
| --- | --- | --- |
| 改之前 `cargo test --workspace` | 809 passed / 0 failed | **421** |
| 改之后 `cargo test --workspace` | 812 passed / 0 failed（多的 3 条是守卫自己的测试） | **0** |
| 改之后 `ccnm-cli --test cli -- --test-threads=1` | 35 passed | 0 |
| 改之后直接循环跑 `ccnm-core` 测试二进制 20 次，`--test-threads=64` | 20 次都是 639 passed / 0 failed | **0** |

最后一行是守卫最容易出错的场景：删得太早、删到别的测试的目录，只有高并发下才看得出来。按基线推算（单元测试那部分每次约 370 个），改之前这 20 次会留下七千多个目录。

守卫自己的三条测试：目录树和 `also` 的路径在 drop 后都不在了；从没建过的路径不报错；失败（线程 panic）的测试保留目录。

门禁：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`git diff --check`。

## 6. 范围之外

- **被 SIGKILL 或超时杀掉的测试进程**留下的目录：Drop 跑不到，没有 `unsafe` 也没有别的钩子。CI 的 10 分钟超时、人按 Ctrl-C 都属于这一类。
- **失败的测试保留目录**是故意的；循环复现偶发失败时，失败了几次就留几个。
- **本机已有的历史残留**没清：开工时 `$TMPDIR` 下 745 个、`/tmp` 下 168 个，是 P34 清理之后又攒的。清法见 P34 记录（按名字里的 pid 判断进程是否还在）。
- Linux 上没有单独量。仓库里没有只在 Linux 上编译的测试 fixture，改动也不含平台分支；CI 的 runner 用完即弃，残留不累积。
