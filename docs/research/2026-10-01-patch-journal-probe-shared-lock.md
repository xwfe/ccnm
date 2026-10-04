# `apply_patch` 的日志探测改用共享锁（2026-10-01）

接的是 [jobs 偶发记录第 6 节](2026-09-30-jobs-tests-under-load.md#6-看到了没动的)留下的"已验证、未修"。修法与那份记录第 3 节 `retention::in_progress` 的相同（`6f66675`）。**只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0；没有连远端、没有跑模型、没有推送或部署。协议、错误码、报错文字、日志格式都没变。

## 1. 结论

| 事 | 性质 | 处理 | 提交 |
| --- | --- | --- | --- |
| 两次探测撞在一起，后一次把被打断的提交读成"还在提交"，patch 被放行 | 产品缺陷，只会漏拦 | 探测改用共享锁；确定性回归先红后绿 | `415aa0b` |
| 一次 patch 的 `check_abandoned` 把另一次 patch 正在写的日志当成"没写完"删掉，那次 patch 失败 | 产品缺陷，顺带验证出来，当时**没修** | 记进 `observed_gaps`，见第 4 节；2026-10-04 在 [P67](2026-10-04-p67-young-patch-journal.md) 修好 | `e786a19` |

## 2. 缺陷

**背景**：`apply_patch` 改文件之前，先把"要改哪些文件、原件备份在哪"写成一份日志（journal），放在状态目录的 `patches/` 下；整个提交期间，`Journal::open` 一直对这份日志持着**排他锁**（flock），进程不管怎么死，内核都会放掉这把锁。下一次 `apply_patch` 开头的 `check_abandoned` 逐份看这些日志：锁拿得到，说明写它的进程已经不在、提交被打断了，就拒绝这次 patch，并列出可能改了一半的文件。

**缺陷**：[`patch::probe_journal`](../../crates/ccnm-core/src/mcp/patch.rs) 去"试一下锁"时用的也是**排他锁**。探测拿着锁的那一瞬间，另一次探测拿不到，读成"有人正在提交"，`check_abandoned` 跳过这份日志，这次 patch 就盖在一个文件可能互相对不上的工作区上了。`patches/` 是整台机器同一账号共用的，所以"另一次探测"可以来自同一个 `mcp-serve` 的另一条线程，也可以来自另一个项目的会话。

P34 修的是同一把锁的另一个洞（探测期间别的线程 fork，子进程把锁带走，[P34 记录](p34-journal-lock-release-2026-09-17.md)），修法是探测完显式 `unlock`；两次探测撞在一起的这一种当时没堵上。

**先红后绿**：`mcp::patch::tests::a_probe_is_not_what_holds_an_interrupted_journal`。建一份没人持锁的日志（等于提交被打断），在 `probe_journal` 的测试钩子里——也就是第一次探测正拿着锁的时候——再探一次，再发一次真的 `apply_patch`。旧代码：

```text
assertion `left == right` failed: (the probe, one looking at the same moment): neither may read an interrupted commit as in progress
  left: (false, Some(true))
 right: (false, Some(false))
```

失败时留下的工作区里 `src/main.rs` 已经被改成 `let x = 9;`——patch 确实被放行了。修复后这次 patch 被拒、报 `interrupted`、文件没动。

**要撞上需要什么**：两次探测落在同一份被打断的日志上、而且相隔在微秒级（探测持锁只有 open 到 unlock 那几个系统调用）。被打断的日志本身就少见（进程在改名改到一半时被杀），所以真实发生的概率很低；但它错的方向是"该拦没拦"。

## 3. 改法，以及显式 `unlock` 为什么留着

`probe_journal` 改成 `try_lock_shared`。提交持的是排他锁，所以提交进行中共享锁照样拿不到，"是不是还在提交"的判断不变；共享锁之间互不相挡，两次探测不会再把对方当成提交。

**显式 `unlock` 留着**。判断标准是：有没有人会对一个探测可能碰过的、已经存在的日志文件再拿排他锁——有的话，探测留在 fork 出来的子进程里的那份共享锁会挡住它。

- **有**。`Journal::open` 先 `File::create` 出 `<pid>-<id>.json.tmp`、写内容、`sync_all`，**然后**才拿排他锁、改名成 `.json`。这段时间里文件已经在 `patches/` 下，而 `check_abandoned` 会探 `.tmp`。
- 对照：`retention::probe`（`6f66675`）不 `unlock`，因为一条 run 在别人看得见它的 `running` 文件之前就已经锁上了，之后没人再拿排他锁；`write_guard::observe` 是共享锁加显式 `unlock`。
- 但要说实话：**今天去掉这个 `unlock` 几乎不改变任何结果**。探测要是先拿到了某个 `.tmp` 的锁，它会把这个文件判成"没写完"并删掉（第 4 节），那次提交反正失败。留着它，是为了让"探测回答完就什么都不持有"不依赖这个推理，也免得将来修第 4 节时又踩到一把留在子进程里的锁。代码注释写了同样的理由。

**P34 那条用例补了一问**。`a_probe_lets_go_of_the_lock_even_when_a_child_holds_a_copy` 原来只问"下一次探测读到的是不是已中断"；换成共享锁以后，去掉 `unlock` 它也照样绿（共享锁不挡共享锁），用例名说的事就没人验了。现在在子进程还攥着副本时再问一句"写的人拿不拿得到排他锁"。临时把 `unlock` 删掉验过：红的正是这一问，原来那一问仍是绿的：

```text
the probe must let go of the lock explicitly: the child's copy kept it, and it would refuse the exclusive lock a commit takes
```

P34 的两条确定性用例（上面这条和 `an_abandoned_journal_reads_as_abandoned_even_when_a_child_holds_a_copy`）修复后都通过。

**新旧版本混跑时**：旧构建的探测仍是排他锁，它和新构建的探测撞在一起时还是会互相读成"在提交"。升级二进制之后，之前起的 `mcp-serve` 还跑着旧代码，要等它们重启这处修复才完全生效。

## 4. 顺带验证出来的另一个缺陷：别人正在写的日志被当成没写完删掉（没修）

> 2026-10-04 [P67](2026-10-04-p67-young-patch-journal.md) 已修：`check_abandoned` 对没老过一小时的 `.tmp` 不探也不删。下面是当时的记录。

判断第 3 节"`unlock` 留不留"时看到的。`check_abandoned` 对 `.tmp` 的处理是：锁拿得到，就当成"改名之前进程就死了"，直接删掉。可是 `Journal::open` 从建出 `.tmp` 到锁上它，中间隔着写内容和 `sync_all`。另一次 patch 的 `check_abandoned` 落在这段时间里，就会删掉一份正在写的日志；写的那一方随后改名时找不到文件，这次 `apply_patch` 报 `cannot place the patch journal`（也可能先在拿锁时报 `cannot lock the patch journal`）。失败发生在改任何工作区文件之前，暂存文件会被清掉，重试即过。

用一条临时用例量过（没提交）：

| 实验 | 结果 |
| --- | --- |
| 手工建一个没锁的 `.tmp`（等于写的人正处在这段时间里），另一个工作区跑一次 `check_abandoned` | `.tmp` 被删；写的人随后拿锁成功、改名报 `NotFound` |
| 建 `.tmp` → 写 400 字节 → `sync_all` → 拿锁，测 200 次这段时间（负载约 45） | 最短 3.8 ms，中位 5.0 ms，p90 6.0 ms，最长 27.5 ms |
| 一条线程每 2 ms 跑一次 `check_abandoned`，另一条连续 `Journal::open` 2000 次 | 1991 次 `cannot place the patch journal` |

第三行的检查频率是故意调高的，只证明机制。真实频率下是**估算、没测**：这段时间约 5 ms（macOS 上 `sync_all` 走 `F_FULLFSYNC`，所以是毫秒级；Linux 没量），另一个会话每 10 秒发一次 patch 的话，每次 patch 撞上的概率约 0.1 × 0.005 = 0.05%，也就是两个项目同时频繁改文件时，大约两千次 patch 里有一次无故失败一下。比第 2 节修掉的那个容易碰到，但后果轻：失败得干净，模型重试即可。

没修，因为它是提交路径上另一处设计，不在这次范围里。能想到的方向（都没试）：建完文件立刻上锁再写内容（把窗口从毫秒缩到微秒，但没合上）；写的一方改名发现 `.tmp` 没了就重建一次；或者让文件锁好之后才出现在 `patches/` 里。怎么修、要不要修由用户定。

## 5. 验证

起点 `70fc1ab`，代码提交 `415aa0b`。本机 macOS 26.6.2 arm64，同一时间另一个会话也在这台机器上干活，负载 37–66（10 核）。

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace`：22 个测试二进制 1051 passed / 0 failed；`cargo test --workspace -- --test-threads=64`：同样 1051 / 0。这次新增 1 条，改了 1 条。
- `cargo +1.89 check --workspace --all-targets --locked`：通过。
- `python3 scripts/ci_gates.py`：计划检查、协议检查（fixture 46 + 29）、Python 262 条 0 跳过 0 失败。
- `git diff --check`：通过。
- 跑红时失败用例留下的 2 组测试目录、实验用的目录都已删；`$TMPDIR` 下 `ccnm-*` 的总数随另一个会话的测试在变，没法用前后对比说明，只按名字确认了这次的都不在。

**没覆盖的**：Linux 没跑（flock 共享/排他语义两边一样，但没实测）；真机没验；没有用真实二进制加两个 `mcp-serve` 去卡那个微秒级窗口，回归停在 `patch` 模块这一层。

## 6. 为什么没另立阶段

开工和收尾时 main 上 P66 都是 `in_progress`（另一个会话在做），不是 `blocked`。[计划约定](../plan/README.md)规定同一时刻最多一个 `in_progress`，只有前一阶段受阻时才能接着认领，所以这次不能立阶段。这处修的是已完成的 P34 承诺过的行为（"只会漏拦"那一类没拦全），没有新的验收范围；仓库里同类事情的先例是 `observed_gaps` 记一条加一份研究记录（[jobs 偶发那次的第 8 节](2026-09-30-jobs-tests-under-load.md#8-为什么没另立阶段)）。所以 `status.json` 只改 `observed_gaps`：原来那条改成已处理，第 4 节的事另加一条；`current_task`、`handoff` 和各阶段记录都没碰。
