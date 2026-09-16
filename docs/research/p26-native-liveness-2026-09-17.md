# 原生链的 Runtime 侧探活（P26，2026-09-17）

验收见 ROADMAP 的 P26，设计见[双执行入口方案](../plan/runtime-surfaces.md)第 12.3 节，要解决的问题来自 [P24 记录](p24-native-real-machine-2026-09-16.md)的网络黑洞一节。脚本和每轮原始结果在 toexec 仓库的 [`evidence/v2-c/p26-liveness/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/p26-liveness/README.md)。

**一句话结论**：Runtime 上的 `exec-serve` 现在能自己发现 Agent 已经不在了。客户端静默 30 秒就发一个 Codex 不认识、但会老实回错的请求；连续 10 分钟一个字节都没收到，就按正常路径收尾放锁。**代价**：Agent 睡眠或断网超过 10 分钟，原生会话会被结束，Codex 里要 `/exit` 重开。**全程零模型额度**，只在本机验证；hpsrv 上的真机黑洞没有复测（P24 的一次性公钥已撤，要重新授权）。

## 1. 为什么能这样探活（P26.1）

exec-server 协议里没有给客户端的 ping。能借用的是 Codex 0.154.0 客户端的一条行为（源码 `codex-rs/exec-server/src/client_recovery.rs`）：服务端发来它不认识的**请求**，它回 `-32601` 然后照常工作；不认识的**通知**则会让它断开连接。所以探活必须是带 id 的请求，id 用字符串（`ccnm-liveness-<n>`），跟 Codex 自己从 1 开始编号的请求永远撞不上。

源码读出来的行为要实测才算数。做法：在 P23 的本机链路里换一个传输脚本，它每隔 N 秒往 Codex 的 stdin 写一个 `{"id":"ccnm-liveness-<n>","method":"ccnm/liveness","params":{}}`，把 Codex 的回答截下来记日志、不往后转。真实 Codex TUI 跑三轮：第一轮跑命令，然后空闲，第二轮跑 `sleep 12`（让探活落在命令执行中间），第三轮收尾。

| 轮次 | 探活间隔 | 发出 / 回答 | 回答内容 | 最慢回答 | 三轮都完成 | 传输进程启动次数 | TUI 上出现探活相关文字 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| r1 | 5 秒，中间空闲 40 秒 | 13 / 13 | 全是 `-32601`，`exec-server client does not implement \`ccnm/liveness\` yet` | 2 毫秒 | 是 | 1 | 否 |
| r2-fast | 0.25 秒，中间空闲 10 秒 | 142 / 142 | 同上 | 11 毫秒 | 是 | 1 | 否 |

`sleep 12` 执行期间 r1 收到 2 个探活、r2-fast 收到约 48 个，命令都照常退出。第一次跑 r1 时第二轮没发出去，查下来是测试脚本把文字和回车放在同一次 `send-keys` 里，TUI 当成粘贴、把回车变成了换行；和探活无关，改成分两次发之后重跑。

## 2. 做了什么（P26.2）

改动都在 `crates/ccnm-core/src/native/`：

- **`liveness.rs`（新）**：判定规则是一个纯函数 `step(计时, 现在, 上次听到客户端, 上次探活) → 等 / 探活 / 放弃`。静默满 30 秒探活，之后静默期间每 30 秒再问一次；静默满 10 分钟放弃。30 秒取 MCP 心跳 `mcp::server::HEARTBEAT` 的值；10 分钟是用户选的取舍，不可配置。
- **"听到客户端"怎么算**：客户端来的**任何字节**都算，不是整条消息。一个 30 MiB 的 `fs/writeFile` 在慢网上一点点传过来，客户端显然还在。
- **大消息往客户端推进也算**：往客户端写一行时要一直持有输出锁，免得探活或拒绝插进行中间，这期间发不了探活。如果这一行要分多块写，每写完一块就算听到一次：客户端在读，块才写得进去。**只写一块就完的行不算**，因为小写入不管对面在不在都会先落进 ssh 和 TCP 的缓冲区——对死掉的客户端持续输出小行（比如命令还在打印）不能让会话一直活着。
- **`serve.rs`**：转发循环不再阻塞在"等某一边结束"上。读客户端、读执行端、发探活各一个线程，主线程每秒醒一次看时钟。原因是两种阻塞都可能永远不返回：读一个不再来数据的 stdin，和往一个缓冲区已满的死连接写。探活线程通过容量为 1 的通道接活，主线程只 `try_send`，所以探活卡在写上也不会拖住放弃的判断。
- **回答不转给执行端**：客户端发来的消息如果没有 `method`、id 是 `ccnm-liveness-` 开头的字符串，就在进规则表之前丢掉。
- **放弃之后**走和客户端关闭完全一样的收尾：关 exec-server 的 stdin → 等它退出（10 秒不退就杀进程组）→ 按会话标记扫进程 → 扫干净才写 `released`。扫不干净仍然 `held`，放锁条件没变。
- **关 exec-server 的 stdin 最多等 2 秒拿锁**：读客户端的线程转发一行时持有这把锁，如果 exec-server 因为自己的输出堵在死客户端上而不再读 stdin，这次写会永远阻塞。无限等这把锁，就等于把"锁一直 held"搬到了另一个地方。拿不到就不关，交给收尾那一步杀进程组。
- 为了能测，会话主体抽成 `Session::run(客户端, 计时, 扫描器)`，客户端的读写端和计时都能注入；`serve()` 传 stdin/stdout 和默认计时。

## 3. 测试（P26.3）

离线测试（都在 `ccnm-core`，计时注入成 100 毫秒探活 / 400 毫秒放弃）：

| 测试 | 证明什么 |
| --- | --- |
| `liveness::tests` 6 个 | 纯函数：说话的客户端不被问；静默时按间隔反复问、到点放弃；每次都回答的客户端跑 3 个放弃周期也不被放弃（问 59 次）；任何字节重新计时；读到部分消息也算听到；只有探活 id 的回答会被截下 |
| 回答探活的客户端 | 假执行端 + 本地套接字客户端，活过 5 个放弃周期，回答了 10 次以上；执行端日志里有 `initialize`、没有任何 `ccnm-liveness` |
| 从不回答的客户端 | 400 毫秒到 2.4 秒之间结束，原因 `ClientSilent`，结束前至少问过 2 次 |
| 慢读的大消息 | 执行端发一行 1.5 MB，客户端每秒只读 1 MB、从不写：传输时间超过放弃时间 2 倍，期间不结束；传完之后再静默 400 毫秒才结束 |
| 不读的大消息 | 同样的执行端发 8 MB，客户端一个字节都不读：写被堵住、输出锁一直被占、发不出探活，仍然按时结束 |
| 不读的小输出 | 执行端每 20 毫秒打印一行，客户端不读：照样按时结束 |
| 整个会话 | 真写锁 + 假执行端 + 录下的真实 `process/start` 起一条 `sleep 600`，客户端之后不再说话：`ClientSilent` 结束，带标记的进程一个不剩，锁文件是 `released`，生成的 `CODEX_HOME` 已删 |

写测试时踩到两个测试本身的错，都不是实现的问题：单测里一处期望写反了（字节到达 100 秒后应该再问，不是等）；"慢读大消息"的假执行端最初用 `exec cat >/dev/null`，把 stdout 管道关了，大行一传完转发循环就读到执行端 EOF 结束，看起来像被误判。

## 4. 真实二进制，默认计时

ccnm 是 P26 提交 `1d6ac20` 的 debug 构建，Codex 0.154.0，计时不注入（30 秒 / 10 分钟）。四轮：

| 轮 | 客户端 | 过程 | 结果 |
| --- | --- | --- | --- |
| `silent-defaults-start-rejected` | 直连 `exec-serve` 的脚本：握手、发录下的 `process/start`，之后一个字节都不发 | 第 30.7 秒第一次探活，之后每 30 秒一次，共 19 次 | 最后一个字节之后 **601.0 秒**结束，退出码 0，锁 `released`，生成的 `CODEX_HOME` 已删。**但这条命令没起来**：录下的请求里 `threadId` 是占位符 `<thread>`，真实执行端回 `-32602`（不是 UUID），所以这一轮"无残留"不算数 |
| `silent-defaults` | 同上，去掉占位的 `metadata`（ccnm 自己的真实执行端测试也是这样做的） | 执行端回 `processId: p2`、`sandboxType: macosSeatbelt`，`sleep 3593` 在静默期间一直在跑；第 30.4 秒第一次探活，之后每 30 秒一次，共 19 次 | 最后一个字节之后 **600.9 秒**结束，退出码 0；结束 1 秒后原来那条 `sleep` 已不在，锁 `released`，生成的 `CODEX_HOME` 已删。汇总里"结束后仍在跑"列出过另一个 pid，查下来是我等结果的那个 shell——它的脚本文本里含 `sleep 3593`，`pgrep -f` 按整条命令行匹配就把它算进去了；脚本已改成按进程名和完整参数匹配，并用诱饵进程验证过 |
| `frozen-defaults` | P23 原样链路上的真实 Codex TUI。第一轮留下 `sleep 3594` 在跑，然后给 TUI 整棵进程树（4 个进程）发 SIGSTOP——从 Runtime 看，就是笔记本合盖 | 冻结前 18.2 秒是最后一条客户端消息；冻结期间 19 次探活，间隔都是 30 秒 | 冻结后 583.2 秒、即最后一条消息之后 **601.4 秒**结束，退出码 0，`sleep 3594` 已不在，锁 `released`。SIGCONT 之后 **TUI 上没有任何提示**；第二轮模型调命令，收到 `exec_command failed: CreateProcess { message: "Rejected(\"Failed to create unified exec process: exec-server transport disconnected\")" }`，Codex 不重连，全程只有 1 条连接 |
| `frozen-120s` | 同上，只冻 122.6 秒 | 冻结期间 4 次探活没人回答，锁保持 `held`、`sleep 3594` 还在跑 | SIGCONT 的**同一时刻** 4 个回答全部到达（都是 `-32601`）；第二轮命令正常执行、输出 `after-resume`；仍是 1 条连接。关掉 Codex 后走 `ClientClosed` 正常收尾，锁 `released`，无残留 |

stderr 里放弃那一段是（去掉颜色和时间戳）：

```text
WARN nothing from the client, not even an answer to a liveness request; ending the exec-server session silent_seconds=600
INFO exec-server relay ended end=ClientSilent
INFO exec-server session ended; write guard released session=<session id>
```

## 5. 门禁

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 通过。
- `cargo test --workspace`：18 个测试套件、771 个测试全过，其中 `ccnm-core` 614 个（新增 12 个：`liveness::tests` 6 个、`serve::tests` 6 个）。
- 本阶段没改 Python helper、协议文档和计划脚本以外的东西；`python3 scripts/check_plan.py` 在收尾提交时跑。

## 6. 没做到、没测到的

- **真机网络黑洞没有复测。**P24 的一次性公钥已撤，hpsrv 上的 ccnm 仍是 P24 的 `7ae2d4b` 构建（没有探活）。本机用 SIGSTOP 模拟的是"连接还在、对面不说话"，和包被丢掉的区别在 TCP 层：黑洞里探活写进去的数据会一直重传，Linux 默认大约 15 分钟（`tcp_retries2 = 15`）才判连接死，比 10 分钟晚，所以结论应当一致，但没有实测。
- **10 分钟不能配置。**用户选的是固定取舍；要配置得先有真实需求。
- **慢读大消息的判定是启发式的。**每写完一块算一次听到，理论上一个不读的客户端，能让写入在填满 ssh 和 TCP 缓冲区之前（几 MB）一直"推进"；要持续拖住会话，执行端得每隔不到 10 分钟就往死客户端发一条超过 64 KB 的消息。没见过这种负载，测试里的不读大消息（8 MB）和不读小输出都按时结束。
- **旧构建没有这个行为。**hpsrv 上的 `7ae2d4b`、各机器上装的 0.7.0（后者本来就没有原生链）都要等下一次发版和替换。
