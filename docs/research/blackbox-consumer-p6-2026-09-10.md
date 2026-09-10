# 黑盒消费者与兼容规则（2026-09-10）

第一个真正按协议写出来的客户端、它驱动的契约测试、以及作为消费者用下来的反馈。

真机双机闭环（P6.3）按用户决定推迟，见第五节。

## 一、客户端

[clients/python/ccnm_machine_client.py](../../clients/python/ccnm_machine_client.py)，约 260 行，只用 Python 标准库。

**为什么是 Python 而不是 Rust。** 用 Rust 写这个客户端，最省事的做法永远是 `use ccnm_core::...`，而 P6.1 要证明的恰恰是"不链接任何 ccnm crate 也能用"。Python 在物理上做不到链接，所以这个证明是结构性的，不靠自觉。仓库本来也有 Python 测试基础设施。

它做的事就是 spawn `ccnm rpc`、往 stdin 写一行 JSON、从 stdout 读一行。有一条测试把这个文件复制到一个跟仓库无关的目录再跑一遍，证明"复制走就能用"不是嘴上说的。

一问一答，所以天然不会踩背压那个坑。文件开头写清楚了：改成连发多条就必须另起线程读，否则两边一起卡死。

## 二、契约测试覆盖了什么

20 个用例，全部只走字节流。P6.2 点名的场景逐条对应：

| 场景 | 怎么测的 |
| --- | --- |
| provider 错配 | 换 instance 能选到另一个 provider 的 instance；换 node 一律 `-32009` |
| 旧 peer | 假对端只说 `ccnm.machine/9`，客户端收到 `-32002` 和对方支持的列表 |
| 重复启动键 | 同键同输入返回同一个 session；同键不同输入 `-32010` 并指出原来那个 |
| 启动响应丢失 | 假对端答完握手就消失，客户端报连接中断而**不是**启动成功 |
| 重连 | 新连接用 session id 查得到 |
| RPC 重启 | 启动键跨服务端进程仍然认得 |
| 取消中 | 注入一个 owner 已死的运行中记录，状态是 `unknown`，stop 幂等且不宣称完成 |
| Runtime 不可达 | 配置指向 `.invalid`，会话以终态收场 |
| 输出截断/过期 | 输出带 `truncated`/`cursor`；任何游标都 `-32012` |
| 不重复执行 | 幂等键那条断言两次调用拿到同一个 session |
| 不串 session | 两个 workspace 各自的结果不混 |
| 不泄漏凭据 | 所有响应序列化成一个字符串，逐个断言 home 路径、`.claude`/`.codex`、`auth.json`、`CLAUDE_CONFIG_DIR`、私钥文件名、`session_dir`、`controller` 都不在里面 |

坏对端由 [fake_rpc_peer.py](../../tests/fixtures/fake_rpc_peer.py) 扮演。真服务端不会宣称自己说别的协议版本，也不会答应了握手就消失，而客户端必须扛住这些——要验证就得有个愿意演的对端。

第三种扮演是那个已知的坑：**收到不认识的东西就以退出码 0 结束、什么都没做**。测试断言客户端把它当连接中断，并且确认对端真的返回了 0——进程"正常"退出不是协议成功。

只有一处碰了内部状态：取消中那条要把记录改回运行中。客户端造不出一个"正在跑且 owner 已死"的会话，而这个场景必须验。测试里写明了这是唯一的例外。

## 三、作为第一个消费者的反馈

### 够用的

六个方法写一个 Orchestrator 的执行层够了。`start_key` 的语义清楚，`effect` 比任何 `retriable` 布尔都好用——`none` 就重发，`unknown` 就去查现场，不用猜。

`session.result` 在未完成时返回状态而不是错误，让轮询代码只有一条路径。

### 想要但决定不加：`session.list`

写 `wait()` 的时候第一反应是"要是丢了 session id 怎么办"。协议没有列表方法，客户端必须自己持久化 id。

**不加。** 理由不是嫌麻烦：Orchestrator 本来就要持久化任务和 attempt（那是它的 O1），session id 是那条任务记录的一部分。给一个 `session.list` 会诱使调用方拿它当真相来源去枚举、去对账——那就是第二份真相，正是两个产品边界要避免的东西。

作为补偿，协议文档写清楚了后果，并给了直接建议：**只要这次执行的结果对你有意义，就给它一个 `start_key`**。丢了 id 还能靠键找回来。

### 用起来别扭但正确的

`agents.list` 不返回 provider。想按"我要一个 Codex"选 Agent 的客户端得靠 instance 命名约定（`codex-main`）或者先跑一次拿到 `agent.provider`。

这个别扭是真的，但改不了：provider 由 Agent Node 权威解析，Runtime Node 上的服务端结构上不可能知道（[P5 记录](machine-api-p5-2026-09-10.md)有推导）。让服务端猜或者去 SSH 问一趟，比这点别扭糟得多。

## 四、兼容规则

协议说明第 13 节写死了三张清单。最值得记的是这一条交换：

**终态集合冻结在 `completed` / `failed` / `unknown` 三个**，换来客户端可以安全地把不认识的 `state` 当作"还没结束"继续轮询。新加的状态一定是过程中的，所以等下去总会走到这三个之一。

反过来做——把不认识的当结束——会让调用方以为一个还在跑的任务已经完了，那是最坏的方向。真要第四个终态就是 `ccnm.machine/2`。

假对端的 `--from-the-future` 演一个未来版本的服务端：多返回 capability、多返回字段、报一个 v1 里没有的 `queued` 状态。测试证明客户端照常工作，且 `wait()` 在那个状态上超时退出而不是误判结束。另一条测试把客户端的终态集合和说明文档里那句话钉在一起，改一边不改另一边就会红。

## 五、P6.3 推迟

真机双机闭环需要重建 P3 那套临时环境（Runtime 账号、SSH 准入、两端部署）、跑真实 Claude/Codex 消耗订阅额度、完事再清理归零。P3 那次这个流程来回了十几轮。

**用户决定：推迟，和 P7.3 的发布前 dogfood 合并成一次真机。** 理由是 P7.3 本来也要求真实项目走完启动→修改→测试→结果→停止/恢复，两次真机合成一次只重建一遍环境。

执行顺序不变，也不需要改验收编号：环境搭起来之后**先跑 P6.3**（用公共 API 各跑一次真实 provider，与人类 CLI 结果对照）收口 P6，再进 P7 跑 P7.3。同一次环境，两个阶段。

代价写明白：**v1 兼容承诺要晚一个阶段才敢确立**。规范、schema、golden 和兼容测试都已经提交，但 P6.4 的落点是"确立 v1"，那一步依赖 P6.3 的真机结果，所以 P6.4 和 P6.3 一起留在 blocker 里。

### 当天晚些时候：上面这段的执行安排被改掉了

上面写的是"P6 留着不收口，等真机跑完 P6.3 再进 P7"。用户随后决定**把 P6.3 的真机部分正式移交 P7.3**：验收编号一个没删，只是移动了执行位置，P6 当场按 completed 收口。

差别在于 P6 的状态：上面那版 P6 会一直挂着，改后 P6 已完成，而协议明确记成 **v1 候选**——正式确立随 P7.3 的真机结果。条文见 [ROADMAP 的变更说明](../plan/ROADMAP.md)，执行计划见 [P7.3 真机会话计划](../plan/p7-real-machine-session.md)。

## 六、验证

```text
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_blackbox_client -q   20 passed
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -q            74 passed
python3 scripts/check_protocol.py                                            通过（38 个 fixture）
python3 scripts/check_plan.py                                                通过
git diff --check                                                             通过
```

本轮没有修改 Rust，不重跑也不重报 Rust 门禁数字（上一轮是 596）。

**没有验证的**：真实 provider、双机、与人类 CLI 的结果对照、订阅额度消耗下的行为。全部留给合并后的那次真机。
