# `apply_patch` 日志锁：探测完显式放锁（P34，2026-09-17）

验收见 ROADMAP 的 P34。缺陷是修两条并发测试的分支（`claude/ecstatic-bose-c1e7c7`，提交 780e8eb、31a5cce，2026-09-17 合并进 main）顺带查出来的，原始记录在 `status.json` 的 observed_gaps。

**一句话结论**：`apply_patch` 判断上一次提交有没有被打断，看的是它的记录文件（journal）的 flock；探测拿到锁之后原来靠关文件放锁，现在先显式 `unlock` 再关。修复前，探测期间别的线程一 fork，锁就被子进程手里的 fd 副本延长到它 exec 为止，紧接着的下一次探测会把已中断的记录当成"还在提交"而放行这次 patch；修复后同一场景下一次探测读到的是已中断。只会漏拦、不会误报，改动只有放锁的时机，判据、错误码、报错文字、记录格式都没动。零模型额度，本机 macOS。

## 1. 缺陷

`crates/ccnm-core/src/mcp/patch.rs` 的 `still_running`：打开 journal、`try_lock`，拿到就说明写它的进程已经不在（`Journal::open` 在整个提交期间持锁，进程一死内核就放），然后函数返回、文件关掉。

flock 挂在**打开文件描述**上，关文件只在指向它的所有 fd 都关掉之后才放锁。Rust 打开的文件都带 CLOEXEC，但那只在 exec 时生效：同一个 `mcp-serve` 里 `exec_command`、`list_files` 会 fork，fork 出的子进程在 exec 之前完整持有父进程的 fd 表，包括这把锁的副本。fork 恰好落在"探测拿到锁"到"关文件"之间时，锁就活到那个子进程 exec 为止——几毫秒到几十毫秒，负载高时更长。这段时间里的下一次探测（同一个进程的下一次 patch，或共用状态目录的另一个 `mcp-serve`）拿不到锁，`check_abandoned` 把这份记录当成有人正在提交而跳过，patch 放行。

被 `abandon` 保留的 `Journal`（回滚失败、工作区已知不一致）在 drop 时同样靠关文件放锁，同理：它被保留就是为了让下一次 patch 拒绝，而下一次 patch 在这个窗口里不会拒绝。

分支上先用 Python 验过机制：只 `close` 时，没 exec 的子进程仍占着锁；先 `LOCK_UN` 就立即能拿。`WriteGuard`（`write_guard.rs`）早就是先显式 `unlock` 再关。

## 2. 改了什么（P34.2）

- `still_running` 改成调 `probe_journal(path, while_held)`：拿到锁 → 跑钩子 → **显式 `unlock`** → 返回"不在运行"。`unlock` 失败只写一条 warn，返回值不变（锁没放掉的后果和修复前一样，不会更糟）。钩子只给测试用，产品传空闭包。
- `impl Drop for Journal`：删文件（或保留）之后显式 `self.locked.unlock()`。

`LOCK_UN` 作用于打开文件描述本身，所以副本在谁手里都一起放掉。

## 3. 测试（P34.1、P34.3）

**不靠并发碰运气**：把 fork 窗口做成确定的。`child_holding_a_copy(file)` 用 `Stdio::from(file.try_clone())` 把描述符的一个副本当 stdin 交给 `sleep 60`——和 fork 到 exec 之间子进程手里那份是同一个打开文件描述，只是活得更久、可控。

- `a_probe_lets_go_of_the_lock_even_when_a_child_holds_a_copy`：先在单独一个文件上确认前提（拿锁 → 副本交给子进程 → 只关文件 → 新句柄拿不到锁）；然后对 journal 调 `probe_journal`，钩子里把副本交给子进程，再调一次 `still_running`，断言读到的是已中断。前提用单独的文件，是因为那个句柄也会被别的测试线程 fork 出的子进程复制，杀掉 `sleep` 之后锁还可能延长几毫秒，不能让 journal 那一步等它。
- `an_abandoned_journal_reads_as_abandoned_even_when_a_child_holds_a_copy`：`Journal::open` 后把 `locked` 的副本交给子进程，`abandon` 再 drop，断言文件还在且 `still_running` 读到的是已中断。

修复前两条都红：

```text
thread 'mcp::patch::tests::a_probe_lets_go_of_the_lock_even_when_a_child_holds_a_copy' panicked at crates/ccnm-core/src/mcp/patch.rs:3227:9:
the probe must let go of the lock explicitly: the child's copy kept it, and the next probe called an interrupted commit in progress
thread 'mcp::patch::tests::an_abandoned_journal_reads_as_abandoned_even_when_a_child_holds_a_copy' panicked at crates/ccnm-core/src/mcp/patch.rs:3263:9:
dropping a kept journal must let go of its lock explicitly, or the child's copy keeps it and the next patch goes ahead
test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 637 filtered out; finished in 0.02s
```

修复后：直接跑 ccnm-core 测试二进制 `mcp::patch::tests --test-threads=64` 40 次、`mcp:: --test-threads=64` 20 次，0 次失败（这是分支上原来复现并发失败的跑法）。

**顺带的测试卫生**：`the_write_policy_is_the_one_the_read_tools_use` 原来把 `outside.txt` 写在 `$TMPDIR` 根下、不带 pid，几个测试进程同时跑会互相截断（分支上 6 路并行 240 次挂 2 次）；现在 root 放在本测试目录下一层，`../outside.txt` 落在自己的目录里。

## 4. 记录了、没修的

同一条 observed_gaps 里另外两件：

- **测试 fixture 目录跑完不删**。43 个文件、86 处 `std::env::temp_dir()` 建目录，按 pid 命名、没有 teardown，每跑一次测试二进制留一批。本轮开工时 `$TMPDIR` 里有 84,889 个 `ccnm-*` 条目；修复后循环 60 次又多了一批。清了进程已退出的那些（数字见 status.json），代码没改：要改就是给 fixture 加 Drop 守卫，86 处都要过一遍，单独立阶段。
- **`read`/`patch` 两条墙钟阈值测试在超额负载下超时**。只在 6 路并行以上出现，默认 `cargo test --workspace` 没挂过；不改。

## 5. 门禁（P34.4）

macOS 26.6.2 arm64：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`（809 passed / 0 failed，比 P33 多两条）、`cargo +1.89 check --workspace --all-targets --locked`、`python3 scripts/check_plan.py`、`git diff --check`。没跑真机、没耗额度、没换任何机器上的二进制。
