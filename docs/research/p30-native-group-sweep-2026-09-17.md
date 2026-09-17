# 原生链：放锁前清空执行端的进程组（P30，2026-09-17）

验收见 ROADMAP 的 P30，要修的缺陷见 [P29 记录](p29-native-gates-2026-09-17.md)第 5 节。实测脚本沿用 toexec 仓库的 [`evidence/v2-c/p29-gates/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p29-gates/README.md)，修复后的结果单独放在它的 `runs/p30-after-fix/`。

**一句话结论**：`exec-serve` 收尾时，除了按会话标记找残留进程，现在也把还留在 exec-server 进程组里的进程找出来杀掉，确认没有了才放锁。修复前 exec-server 在一次带沙箱的文件写入中途被强杀，fs helper 会活过放锁（macOS 20/20）；修复后同一场景 20/20 在放锁前被清掉，会话结束从约 0.17 秒变成约 0.4 秒。全程零模型额度，只在本机 macOS 上用真实 exec-server 验证；Linux 分支只经编译检查和 CI 上的单元测试。

## 1. 改了什么（P30.2）

改动在 `crates/ccnm-core/src/native/serve.rs`：

- **扫描判据**：原来的 `marked_processes(marker)` 改名 `session_processes(marker, group)`，一个进程只要**带这个会话的标记，或进程组号等于 exec-server 的 pid**，就算这个会话的。exec-server 由 ccnm 以 `process_group(0)` 启动，所以它的进程组号就是它的 pid；它死后，组里还有成员时这个号不会被系统复用。
- **跳过僵尸**：僵尸已经结束、不能再写，而它在被父进程回收前一直保留进程组号。原来按标记找时僵尸本就读不到环境（Linux 的 `/proc/<pid>/environ` 为空，macOS 的 `ps -E` 只显示 `<defunct>`），按组找就得显式跳过，否则 helper 的父进程不回收时会一直等到超时、锁保持 `held`。
- **平台分支**：仍只有这一个函数按 OS 分支。Linux 从 `/proc/<pid>/stat` 取状态和进程组号；解析写成不分平台的 `stat_state_and_group`，命令名里带空格和 `)` 时从最后一个 `)` 往后数字段，本机单测覆盖。macOS 的 `ps` 多取 `pgid`、`stat` 两列。所以 P28 的 msrv job 只跑 Linux 的前提没变，`ci.yml` 里那句注释跟着改了函数名。
- **放锁条件不变**：找到的逐个 `kill -KILL`，每 100 毫秒再扫，5 秒内扫不干净就报错、锁保持 `held`。报错里多一句"也可以按进程组 <号> 找"。
- **进程组号复用的风险**：只存在于"组已经空了、同一个号又被新进程拿去当自己的 pid 并自立一组"的这几秒里，和按 pid 杀带标记进程时 pid 被复用是同一量级；写在 `Sweeper::sweep` 的注释里。

## 2. 测试

**先红**（P30.1）：`tests/fixtures/fake_exec_server.py` 加开关 `FAKE_EXEC_LEAVE_HELPER=<pid 文件>`，配合已有的 `FAKE_EXEC_CRASH_ON`，崩溃前先起一个环境为空、留在自己进程组里的 `/bin/sleep 300` 扮演 helper。集成测试 `an_executor_killed_mid_file_operation_leaves_no_helper_behind_twenty_times` 经真实二进制跑 20 轮：发 `fs/writeFile` 让假执行端崩溃，等 `exec-serve` 退出，断言那个子进程已经不在，再开下一个会话。修复前的输出：

```text
thread 'an_executor_killed_mid_file_operation_leaves_no_helper_behind_twenty_times' panicked at crates/ccnm-cli/tests/exec_serve.rs:744:9:
round 0: the helper outlived its executor and the guard was released anyway
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 11 filtered out; finished in 0.49s
```

**后绿**：修复后 20 轮全过。另加两个单元测试：

- `the_sweep_finds_a_process_left_in_the_executors_group_without_the_marker`：真起一个组长和一个清空环境、加入该组的 `sleep`，杀掉组长后，按标记找不到它、按组找得到，扫描杀掉它并报告干净。
- `a_proc_stat_line_gives_state_and_group_whatever_the_command_is_called`：普通行、命令名带空格和括号、僵尸、截断的行。

原有的 `what_cannot_be_listed_or_killed_is_never_reported_clean` 按新签名保留：列不出进程表、杀不掉，都绝不报告干净。

## 3. 真实二进制（P30.3）

修复后的 release 构建（`c80b0b1` 加上本次改动，随后提交为 `6437528`），真实 Codex 0.154.0 exec-server，重跑 P29 的在途场景：

| 场景 | 轮数 | 修复前（P29） | 修复后 |
| --- | --- | --- | --- |
| `helper-crash`：往 FIFO 带沙箱写让 helper 卡住，SIGKILL exec-server | 20 | helper 活过放锁 20/20，放锁后写入到达 20/20 | helper 在放锁前被清掉 20/20，没有写入到达；触发到 `exec-serve` 退出 0.36–0.41 秒（原 0.17–0.22） |
| `helper-close`：同上，客户端正常断开 | 20 | 干净 20/20 | 干净 20/20，0.18–0.20 秒 |
| `client-leaves`：请求在途时断开 | 20 | 干净 20/20 | 干净 20/20，0.17–0.20 秒 |
| `terminate`：终止一棵进程树 | 5 | 同组 0.08–0.09 秒消失，脱离的会话结束时被扫 | 同组 0.07 秒消失，脱离的会话结束时被扫，锁 `released` |

所有轮次退出码 0、锁 `released`、没有带标记的残留。

## 4. 门禁

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：787 passed / 0 failed（P29 时 784，新增 3 个）
- `CCNM_TEST_CODEX_BIN=/opt/homebrew/bin/codex cargo test -p ccnm-cli --test exec_serve against_the_real`：1 passed
- `cargo +1.89 check --workspace --all-targets --locked`，以及加 `--target x86_64-unknown-linux-gnu`：通过、0 条警告
- `python3 scripts/check_plan.py`、`git diff --check`：通过

## 5. 没做到、没测到的

- **Linux 分支没在本机跑过。**`/proc/<pid>/stat` 的解析有不分平台的单测，读 `/proc` 的那几行只经 1.89 的 Linux 目标编译检查；本机没有 Linux 目标的 clippy（1.89 工具链没装 clippy，stable 没装 Linux 目标，没为此改环境）。推送后 CI 的 `linux-runtime` job 会跑到它，**本阶段没有推送**。
- **Linux 上的 fs helper 不在这个进程组里。**按源码它由 `codex-linux-sandbox` 套 `bwrap --new-session --die-with-parent` 启动（`linux-sandbox/src/bwrap.rs`），靠 exec-server 死时的 parent-death 信号结束，本次修复够不着它，也没有实测（P29 第 5 节）。
- 修复后的真实二进制只测了 macOS，没有上真机。
