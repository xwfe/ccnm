# 本地 stdio Machine API 实现记录（2026-09-10）

`ccnm rpc` 的实现、它反过来改掉的三处协议契约、以及**没有**被验证的东西。

契约见 [协议说明](../protocol/README.md)；本文只记实现侧的决定和证据。

## 一、做了什么

| 落点 | 内容 |
| --- | --- |
| `rpc/wire.rs` | JSON-RPC 2.0 信封、有界 framing、错误码表、ccnm 错误到公开码的映射 |
| `rpc/store.rs` | session 记录、start_key 索引、owner 存活判定 |
| `rpc/session.rs` | 四个 session 方法、真实执行器 `SystemRuns`、RFC 3339 时间戳 |
| `rpc/mod.rs` | 读循环、握手状态机、方法分发、`agents.list` |
| `ccnm-cli` | `ccnm rpc` 子命令；`tests/rpc.rs` 的 11 个真实二进制集成测试 |

方法齐了六个：`hello`、`agents.list`、`session.start/status/result/stop`，只支持 `print` 模式。

## 二、三个架构决定

### 1. 执行入口抽成 trait，而不是直接调 launcher

`Runs` trait 有两个方法：`run_print` 和 `stop`。生产实现 `SystemRuns` 调的是 `launcher::run_print_with_agent` 和 `launcher::stop_selected`——**人类 CLI 走的同一批函数**，不是 spawn 一个 `ccnm` 再解析它打印的话。解析 CLI 文案会把措辞冻结成 API，还会丢掉措辞四舍五入掉的东西。

抽成 trait 是为了测试：注入一个立刻返回的替身，于是没有一个测试会启动 Agent 或拨 ssh。

### 2. 协议 handle 不是 ccnm 自己的 session id

ccnm 的 session id 由 **Agent 侧**在 `work::run` 里生成（`session::new_id()`），而 `work::run` 一直阻塞到整个 session 结束才返回。协议要求 `session.start` 拿到 handle 就返回，所以那一刻根本没有 ccnm session id 可用。

于是 RPC 自己发一个 `s-<uuid>` 作为 handle，并在运行结束后把 Agent 报回来的 ccnm session id 记进同一条记录（`finish.ccnm_session`）。**不是两份真相**，是一次运行的两个名字，映射持久化在记录里。

被否掉的替代方案：给内部 `RunRequest` 加一个可选 session id 字段，由 Runtime 侧生成并下发，这样两个 id 就能合一。它更干净，但要改内部协议、要处理旧 peer 忽略该字段的情况，还要在 Agent 侧严格校验一个来自网络的字符串——那个字符串会被当成目录名。本轮范围是"实现 rpc"，不是"改两端协议"，留给后续。

### 3. `session.stop` 按 workspace 寻址

紧接上一条：正在跑的 print session 还没有 ccnm session id，所以 stop 只能按 workspace 提给 Agent 侧。

**这仍然是精确的**，理由是 P3.3 的写入 guard：同一棵工作树同时最多一个受管写 session。"这个 workspace 的 session"就是它。Agent 侧在报告停止之前做进程组核验（P3.2 的成果），所以"停了"这个结论不是猜的。

代码注释里写明了这个依赖——哪天写互斥的语义变了，这里要跟着改。

## 三、实现反过来改掉的三处契约

P4 的草案是纸上写的，写实现才发现有些字段服务端**根本给不出**。三处都是同一个根因：**provider 和 profile 由 Agent Node 权威解析，Runtime Node 不保存第二份**，而 `ccnm rpc` 跑在 Runtime Node 上。

### 1. `agents.list` 的 `provider` 和 `capabilities`：删掉

一开始改成可选，写完测试才发现那个分支根本执行不到：配置校验要求 instance workspace 的 root 只在它的 Runtime Node 上定义（`instance.rs:190`），又要求 instance 模式非 colocated（`instance.rs:216`），两条加起来，绑定里的 node 永远不是本机。

留一个永远缺席的字段，只会让调用方写一段永远不执行的分支，所以从 schema、文档和 fixture 里删掉。

### 2. `agent_public` 的 `provider`：改成可选

`session.start` 一拿到 handle 就返回，那一刻 Agent Node 还没回过话。等 `session.result` 拿到它自己报的身份，`provider` 才有。

### 3. `running` 的定义：从"Agent 在跑"改成"已经交给执行链路，还没结束"

服务端观察不到 Agent 什么时候真的开始推理——`run_print` 是一次阻塞调用。按原措辞，`running` 要么永远不出现，要么是撒谎。改定义比造一个观察不到的状态诚实。

## 四、没有被验证的东西

写在前面，免得把离线绿灯当成验收：

1. **没有跟真实 Agent 跑过一次。** 单元测试注入替身执行器；集成测试跑真实二进制，但每次调用要么在本地预检就失败、要么只读配置。真 provider 的双机闭环是 P6.3。
2. **Runtime 写互斥没有 RPC 层面的独立验证。** 它是结构上继承的——RPC 走 `launcher` 的同一条路，所以受同一个 guard 约束——但没有一个测试证明两个 RPC 客户端抢同一棵工作树会被拒。那需要两台机器。
3. **取消的三条判据同理。** 进程组、MCP transport、写入 guard 的核验都在 Agent 侧，RPC 只负责不提前报成功（`stopping` 不是终态，有测试）。
4. **`interactive` 没有实现**，也不在 `capabilities.modes` 里。
5. **输出不分页、结果不过期。** 一次给最后 8 KiB；任何游标都返回 `-32012`，因为这个 build 从没发过游标。契约允许分页，实现只做了单页——实现得比契约少，不是反过来。

## 五、几个值得记的实现细节

**framing 手写而不是 `Read::take` + `read_until`。** 跳过超长行时那个字节上限要跨调用存活，而 `Take` 会消费掉它包住的 reader，在循环里借不出来。

**`start_key` 用 `create_new` 抢占。** 两个 `session.start` 同时用一个键，正好一个建得成链接文件，输的那个读回已有的 session id 去复用。启动输入原样存不做哈希：协议说"逐字节相同"，而哈希碰撞会让两个不同任务看起来像同一次复用——那恰好是绝对不能起第二个 Agent 的场合。

**owner 同时记 pid 和 `ps` 报的启动时间。** 只记 pid 会被回收的号码骗过去，让死掉的运行看起来还活着。`ps` 本身读不到时是 `Unverifiable`：不知道不等于没了，但对"能不能证明它还在跑"这个判断，两者都是不能。

**服务端死后留在 `running` 的记录报 `unknown`，不报 `failed`。** Agent 在另一台机器上，八成已经跑完了；报失败会诱使调用方重试一份可能已经改过工作树的活。

**RFC 3339 手写而不是引日期库。** UTC 只需要算术，没有时区表。闰年、2000 年的 400 年例外、跨年最后一秒都有测试——手写日历出错就出在这几处。

**背压是 stdio 的固有性质，不是 bug。** 只写不读的客户端会和服务端一起卡死：缓冲满了服务端阻塞在写，于是不再读 stdin。协议文档写清了两种安全写法，集成测试用一个写线程加主线程读证明服务端这一端没问题。

## 六、验证

```text
cargo fmt --all --check                                 通过
cargo clippy --workspace --all-targets -- -D warnings   通过
cargo test --workspace                                  596 passed / 0 failed
  其中 rpc 单元测试                                      71
  其中 tests/rpc.rs 真实二进制集成测试                    11
python3 scripts/check_protocol.py                       通过（38 个 fixture）
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -q   54 passed
python3 scripts/check_plan.py                           通过
git diff --check                                        通过
```

手动跑过真实二进制：`hello`、`agents.list`、未知 session、未知方法、批量、非法 JSON 六种输入，stdout 全是协议、stderr 空、退出码 0、状态目录按预期创建。

一次插曲：有个提交里 clippy 的输出被管道 `tail` 吞掉了退出码，报错没看见就提交了，下一个提交修掉。以后跑门禁不要把 clippy 接进管道。
