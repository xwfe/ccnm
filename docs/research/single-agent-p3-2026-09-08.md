# P3 单 Agent 公共执行离线记录

## 范围和提交

本轮接续 P3，不进入 P4/Machine API，不调用真实 Claude/Codex，不登录、不复制凭据、不部署或替换已安装 Controller。

- `c25d743`：先提交执行/身份/写 guard 契约。
- `238d217`、`08fd76f`：journal 测试增加锁交接检查，随后修正合成 owner 的显式释放；前一个提交本身没有证明不稳定问题修复。
- `3903801`：Runtime MCP 完整生命周期持有写 guard；canonical root、Git common dir、busy、异常 held/unknown、残留子进程与人工恢复。
- `94eb7bf`：公共 instance 选择、v3 binding、Agent-local profile、Controller/supervisor、精确 session 及 colocated 拒绝。

工作区原先的用户修改已在 `dc5f7db`，本阶段起点干净。中断恢复后的文档改动均为本阶段延续，没有纳入其他用户文件。未 push。

## 当前验证

在 macOS arm64 / Rust 1.98.0 上，从当前代码重新执行：

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 511 passed / 0 failed |
| `python3 -B -m unittest discover -s tests -p 'test_*.py' -v` | 19 passed |

测试包括 CLI 30、instance execution 6、MCP read/credential 6、provider safety 2、真实写 guard 子进程 3、core 单元 422、Codex fixture 10、instance config 19、Claude compatibility 5、public lifecycle 3、session identity 5。合计 511，不重复计入定点重跑。

## 证据边界

- 公共默认/覆盖只传同 Node 的 instance id；身份不匹配在 Controller、supervisor、transport、结果读取和 stop 前拒绝。
- 合成 profile 和 auth sentinel 验证目录独立解析；未读取真实认证文件内容。Codex 使用原实测 0.153.4 flags/fixture，没有猜新版本参数。
- Runtime MCP 真子进程测试覆盖并发 owner、正常退出再进入、SIGKILL、残留 exec child、人工恢复；不是网络断线或生产 OS 权限验收。
- Claude golden 原件未重录。colocated 候选参数移除 remote-only 限制，但公共启动明确拒绝，不能宣称真机可用。
- Agent Node 本机 attach/status/result/stop 的原有 CLI 回归保持；试图把所有生命周期绕回 Runtime 曾造成 3 个回归，已撤销。显式 instance/session 校验保留，本机省略选择不承诺重新读取 Runtime 默认值。
- 64 线程压力回归曾发现 journal 和 WriteGuard 的 close 后短暂 busy；诊断观察到第一次 WouldBlock、20ms 后可取锁。fork/exec 继承是与现象一致的解释，并非完成内核级溯源。最终正常释放显式 unlock；异常路径仍保留 held。journal 用例只模拟显式释放，不替代真实异常退出测试。
- 额外 64 线程压力循环中还出现过既有 exec timeout 测试用时 30.5s 的一次失败；未修改该模块，也不声称已解决该高并发问题。当前默认全量门禁通过。

## 未完成项与恢复点

P3 尚未完成，不能因离线通过进入 P4。P3.1/P3.2/P3.3 的代码与离线证据已具备，但完整验收仍需复查真实生命周期和安全边界，尤其是进程组残留、PID 复用、Controller 重启与链路失败后的状态一致性。

P3.5 缺少当前 build 的针对性双机部署/真实 Agent 调用授权及专用 Runtime 执行身份的生产验证。历史 scratch/internal 记录不作为这次公共入口验收。下一轮先确认目标 alias、临时部署位置和不替换现有 Controller 的方案；获授权后再运行真实两 Provider 公共链路并保存脱敏 fixture。账号、ACL、sudo 或防火墙变更须另外确认，不因 dogfood 授权一并执行。

## P3.2 接续：print stop 的进程组确认

本轮工作区起点 `13fd3f5` 干净，范围仅为计划指定的残留进程/PID 复核。

原 stop 在发送组信号后使用 `ps -p <leader> -o pid=`：组长消失不代表同组子进程结束。修复前新回归失败；修复后改用 `ps -axo pid=,pgid=` 核对完整组成员，Agent 与 supervisor 两个组都确认无成员后，才由 stop 补写结束记录。发送 Agent 组信号前同时验证 `pgid == agent_pid` 和 `ppid == supervisor_pid`；已重新挂到其他父进程的 PID 不接受。

定点覆盖：错误父进程无 signal、组长消失但子进程残留、Agent 组和 supervisor 组分别残留、ps 失败/空白/损坏输出不报告结束、既有正常停止与重复停止。状态不明时不结束 supervisor，也不清理 Runtime held marker。当前 macOS 的 ps 数字列格式已本机验证，不采集命令参数或环境。

本轮最终 fmt、严格 clippy、全仓 Rust **512 passed / 0 failed**，Python **19 passed**。新增场景为离线 FakeRunner 回归，不冒充真实跨节点信号验收。父进程核验缩小 PID 误用范围，但不消除观察到发送信号之间的竞态，也不能发现主动脱离进程组的子进程；已有 outcome 快路径与 interactive stop 仍需后续定点复核。P3.2/P3.5 不因此标记完成。

## P3.2 接续：已有结果仍复核 print 进程组

修复前新增测试重现：结果文件存在时，精确 print stop 未查询进程就返回成功，即使 supervisor 或 Agent 组还有成员。现在先分发 print 分支，再对存在的 supervisor/Agent PID 记录逐一复用全组检查。残留成员（包括可能的 PID/PGID 复用）、ps 失败/损坏、PID 空值/非法/越界或记录不可读取均返回 NotReady，不发 signal、不改写既有结果；两组确认不存在后才保留重复 stop 的 killed=false。

兼容边界：旧记录或启动前失败可以没有 PID 文件，继续按无已记录进程处理；这不能证明缺失记录背后的任意进程已结束。interactive 已有 outcome 分支、status 与结果优先级、脱离进程组的子进程、PID 查询竞态仍未解决，本次不扩大完成声明，也不修改 provider 参数或 Claude 默认选择。

验证：新失败用例先红后绿；session_identity 8通过，全量 fmt/严格 clippy 通过，Rust 514 passed / 0 failed，Python 25通过。没有重录 golden，测试为离线 FakeRunner，不替代真机验收。日志 `/tmp/ccnm-p3/outcome-{fmt,clippy,tests}.log` 是本机临时辅助，接续不依赖它们。

本轮网络前置检查在 SSH 建连阶段即 ConnectTimeout，未创建新远端目录，也未开始小块传输。修改前 release build 已完成（9,717,392 bytes），不是上述修复后的可部署构建；恢复网络后必须重新构建再验收。继续保留 P3.5 阻塞和已有临时系统资源清理清单。
