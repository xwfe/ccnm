# P67 `apply_patch` 不删别的 patch 正在写的日志（2026-10-04）

接的是[日志探测改共享锁的记录](2026-10-01-patch-journal-probe-shared-lock.md)第 4 节留下的"已验证、未修"。**只有离线证据**：本机 macOS 26.6.2 arm64、rustc 1.98.0；没有连远端、没有跑模型、没有推送或部署。日志的文件名、内容格式、报错文字都没变；被打断的 `.json` 怎么判断也没变。

## 1. 结论

| 验收 | 结果 | 提交 |
| --- | --- | --- |
| P67.1 回归先红 | `a_journal_still_being_written_by_another_patch_is_left_alone`：旧代码上那份 `.tmp` 被删（`it is not this patch's to remove`） | `e786a19` |
| P67.2 修 | `check_abandoned` 对 `.json.tmp` 先看年龄：没老过 `STALE_TEMP`（1 小时）不探也不删；压测对照旧代码 2000 次全失败、修后 0 次 | `e786a19` |
| P67.3 门禁与文档 | 见第 4 节 | — |

## 2. 缺陷

**背景**：`apply_patch` 改文件前先写一份日志（journal），放在状态目录的 `patches/` 下，这个目录是同一账号所有 workspace 共用的。写法是先建 `<pid>-<id>.json.tmp`、写内容、`sync_all`，再拿排他锁、改名成 `.json`；`.json` 存在就说明日志写全了。每次 `apply_patch` 开头的 `check_abandoned` 逐份看这些文件：`.json` 的锁拿得到说明写它的进程已经死在半路，拒绝这次 patch；`.tmp` 的锁拿得到，就当成"改名前进程就死了"直接删掉。

**缺陷**：建 `.tmp` 到锁上它要经过写内容和 `sync_all`，macOS 上 `sync_all` 走 `F_FULLFSYNC`，这段时间中位约 5 ms（之前量过 200 次：最短 3.8 ms，最长 27.5 ms）。另一个 workspace 的 patch 正好落在这段时间里：

- 它把这份 `.tmp` 删了：写的一方改名时找不到文件，这次 `apply_patch` 报 `cannot place the patch journal`；
- 或者它探测时正拿着共享锁：写的一方拿排他锁被拒，报 `cannot lock the patch journal`。

两种都失败在改任何工作区文件之前，重试即过，所以后果是"无故失败一次"。之前估算两个项目同时频繁改文件（另一边每 10 秒一次 patch）时，大约两千次里撞上一次。

## 3. 改法

`check_abandoned` 遇到 `.tmp`，先看它最后一次写入是不是在 `STALE_TEMP`（1 小时）之前；不是就跳过——不探锁，也不删。老了才和以前一样：锁拿得到就删。

为什么这里可以问时钟，而 `.json` 不行：

- `.json` 存在说明提交已经开始，判断它"被打断没有"决定的是拦不拦下一次 patch，时钟错了就会漏拦或误拦，所以只问锁（`still_running` 的注释写了以前用超时出过什么事）。
- `.tmp` 存在说明提交**根本没开始**，工作区一个字节都没动。删它只是收垃圾。时钟往回跳，垃圾晚一点清；往前跳超过一小时、又正好落在别人那 5 ms 里，那次 patch 干净地失败一次——这和修之前每次都可能发生的事一样，只是从"约两千分之一"变成几乎不可能。
- 工作区里 `.ccnm-…` 临时文件的清理早就是这个规则（`sweep_stale_temps`，同一个 `STALE_TEMP`，理由是"别抢另一个会话正在用的暂存文件"）。年龄判断抽成 `is_stale` 两处共用，按目录项本身算年龄，和原来的 `DirEntry::metadata` 一致（符号链接不跟过去）。

没采用的另外几个方向：建完文件立刻上锁再写内容（窗口从毫秒缩到微秒，但没合上）；写的一方改名发现文件没了就重试（能补救，但删别人文件这件事还在）；改成锁好之后才出现在 `patches/` 里（要改日志的命名和放置方式，范围更大）。

`probe_journal` 里那次显式 `unlock` 留着，注释改了：现在探测只会碰到一小时以前的 `.tmp`，不会再撞上正要上锁的写的一方，`unlock` 留着是为了"探测回答完就什么都不持有"不依赖这个时间判断。

**新旧版本混跑时**：旧构建的 `check_abandoned` 还会删新构建正在写的 `.tmp`。升级二进制后，之前起的 `mcp-serve` 还跑着旧代码，要等它们重启才完全生效。

## 4. 验证

**回归**：在 `patches/` 里放一份刚建、没上锁的 `.tmp`（等于写的一方正处在建文件到上锁之间），在另一个 workspace 里发一次真的 `apply_patch`，然后让"写的一方"继续：拿排他锁、改名。旧代码第一步就失败，`.tmp` 被删；修后 patch 照常成功、`.tmp` 还在、写的一方拿到锁并改名成功。原有的 `a_journal_that_was_never_finished_blocks_nothing`（一份一小时以前的 `.tmp` 不挡 patch、会被清掉）照样通过。

**压测对照**（临时用例，跑完删掉、没提交）：一条线程不停跑 `check_abandoned`，另一条连续 `Journal::open` 2000 次。

| 构建 | `Journal::open` 失败 | 同期 `check_abandoned` 次数 |
| --- | --- | --- |
| 修之前（`d35aefb`） | 2000 / 2000 | 869 088 |
| 修之后 | 0 / 2000 | 666 942 |

检查频率是故意拉满的，只证明机制，不代表真实的撞车概率。

**门禁**：

本机负载 30–38（10 核）。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace -- --test-threads=64` | 1059 通过（并入两个分支后为 1058，新增 1 条） |
| `cargo test --workspace`（默认线程数） | 1059 通过 |
| `cargo +1.89 check --workspace --all-targets --locked` | 通过（`is_stale` 用到的都是 1.89 以前就稳定的 API） |
| `python3 -B scripts/ci_gates.py` | 通过：计划、协议（46 + 29 个 fixture）、Python 262 条 0 跳过 |
| `python3 -m unittest tests.test_check_plan tests.test_check_protocol tests.test_ci_gates` | 48 条通过 |
| `git diff --check` | 通过 |

回归跑红时留下的两个测试目录、压测用例的两个目录都按名字确认后删了。

## 5. 没覆盖的

- Linux 没跑（`sync_all` 在 Linux 上多快没量；改法不依赖它多快）；真机没验；没有用真实二进制加两个 `mcp-serve` 去撞，回归停在 `patch` 模块这一层。
- 写的一方在建文件和上锁之间卡住超过一小时（比如磁盘卡死），它的 `.tmp` 仍可能被清掉；那时这次 patch 本来也成不了。
