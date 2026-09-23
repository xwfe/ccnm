# P51：v0.9.0 完成度、生命周期与文档审计

日期：2026-09-23。起点：`main` / `7f0e01899bb05425d396810c209adcf9dbfc6b60`，工作区与暂存区均干净。Cargo 版本 `0.9.0`；发布记录指向 tag `v0.9.0` / `86bdac0`，本轮没有访问线上 Release 或复验任何已安装二进制。

## 结论与范围

**执行层方向正确，编码闭环已可用；不能宣称完整项目生命周期或可靠无人值守交付已完成。** 进场时状态账本记录的 50 个任务均完成（P0–P46、P48–P50），表示各自范围已收口，不是产品整体完成率。P48/P49 的零额度 Host 测试和 P50 的部分真机补验不能互相代替。

本轮读代码、测试、契约、计划及运维记录，执行本地回归与隔离故障探针，并同步文档。没有修改 Rust 产品代码、重录 fixture、部署、推送、打 tag、修改系统权限或运行付费模型。历史阶段不倒改为未完成；新发现单独列为后续工作。

## C51-01：MCP relay 派生进程未结束，写权已经交出

**优先级：阻断“可靠单写交接”声明；已复现，尚未修复。**

源代码链条：[`mcp/relay.rs`](../../crates/ccnm-core/src/mcp/relay.rs) 的 `stop()` 只在 server leader 尚未退出时发进程组 KILL，且不返回清理成败；`Relay::close_all()` 返回 `()`。[`mcp/server.rs`](../../crates/ccnm-core/src/mcp/server.rs) 收尾时只把 `jobs.stop_all()` 返回的残留接到 `write_guard.abandon()`，relay 收尾不参与这项判定。

隔离探针：[probes/p51-relay-cleanup.py](probes/p51-relay-cleanup.py)。使用本机真实 `target/debug/ccnm`、中立 MCP 客户端、临时 workspace/HOME/state；临时配置显式接受该开发账号未隔离，仅用于故障注入。假 stdio MCP server 启动一个**仍在原进程组内**的子进程，子进程关闭继承的管道、持续写测试文件，server 收到 EOF 正常退出。探针随后打开第二个 coding 会话写入另一个文件。

2026-09-23，macOS 26.6.2 / arm64，实际输出：

```json
{
  "same_process_group": true,
  "first_close_exit": 0,
  "old_child_wrote_after_close": true,
  "guard_markers": ["released\n"],
  "second_writer_succeeded": true,
  "second_close_exit": 0,
  "defect_reproduced": true,
  "owned_child_cleaned": true
}
```

这不是 P43 的“脱离进程组且攥住管道”故障：这里**没有 setsid**，也不需要恶意 server；正常派生后台任务后退出就能触发。已有测试只检查 server 自己的 pid 消失，不能证明其子进程结束。Agent 本机 MCP 使用同一个 `relay::stop()`，存在相同静态风险，但本轮没有为 Agent 侧单独做故障注入，不能说两侧都已复现。

**临时收敛**：对要求可靠交权的项目，在 Runtime 的配置中设 `[runtime_mcp] enabled = false`，结束现有会话后人工核实并清理旧进程树，再允许新 writer。仅修改配置不杀旧进程；这也不解决普通命令已有的 SIGKILL/逃逸后代限制。不要通过删除锁文件来“恢复成功”。

**修复验收**：Runtime relay 的关闭必须有可消费的清理结果；leader 正常退出不能代表全组消失，进程归属未知或信号失败不能报告成功并放锁。覆盖正常 EOF、leader 先退、同组子进程、脱组子进程、超时、取消、server 被杀、空闲回收和下一 writer；Agent 侧复用机制同步复核。不得向已重用 pid 的无关进程发信号。修复后将本探针转换为正式回归：要么旧进程确认结束，要么保留拒绝新 writer 的状态；不能只把日志改得更好看。

## 其他发现与后续优先级

| 编号 / 优先级 | 发现与依据 | 应怎么收口 |
| --- | --- | --- |
| C51-02 / 高 | 新增 Agent MCP 已改变原先“所有执行在 Runtime”的叙述。`agent_mcp.rs` 的本机程序以 Agent 账号执行；清理继承环境后仍加入显式 `env`，不做独立 OS 隔离 | 本轮修正文档；保持本机服务显式 opt-in，明确第三方服务、数据外发与 Runtime 沙箱的范围。不要以字符串黑名单替代账号隔离 |
| C51-03 / 高 | `.github/workflows/ci.yml` 与 `release.yml` 跑 Rust 门禁，但没有执行 Python 中立客户端测试、`check_plan.py` 或 `check_protocol.py`。本地契约要求没有全部落到 CI | 增加自动门禁：先构建真实 CLI，再跑 Python 全套、计划和协议检查；失败应阻断。检查实际产生的工具表/schema，不靠 fixture 自洽冒充行为验证 |
| C51-04 / 高 | 受管会话、外部 MCP、后台命令、第三方 server、Agent 进程分属不同生命周期；SIGKILL 与逃逸后代仍有缺口。写锁也只在相同 state 域生效 | 先补收尾与 ownership 证据，再评估必要的平台进程容器；不以“两个 worktree”或自建上层 lease 假装可以安全并行 |
| C51-05 / 中 | `ccnm.machine/1` 已冻结，但实现没有 interactive、result 分页/过期，`-32008 busy` 不会从 start 返回；大报告只剩尾部 8 KiB | 优先选择真实消费者最需要的 busy/结果完整性，补前后端一致的失败语义和中立客户端证据；不为“填满契约”顺便做 UI 或任务编排 |
| C51-06 / 中 | P48 机器级 skills、P49 Runtime relay 缺完整真实模型/SSH/平台组合证据；P50 的真实模型选用 Agent DeepWiki，不代表 Runtime relay 通过 | 补具名入口的验收，记录真实调用证据、身份、版本、模型和未覆盖项。Codex 0.155.1 的零额度探测不代表受管 pin 已升级 |
| C51-07 / 中 | 原 README 工具数过期，协议入口同时写“草案”和“冻结”；架构仍写 Batch C 待接线；支持矩阵状态列落后于格内真机记录；handoff 仍建议发布已发布版本 | 本轮同步。下一步把有限的当前声明加到文档一致性门禁；不扫描历史研究中的旧数字并一律替换 |
| C51-08 / 中 | Agent MCP 长结果只有内存保留；Runtime 输出和 Machine API 记录生命周期不同；`--purge` 不覆盖 Executor 的输出与所有 RPC 记录 | 文档明确保留/失效/清理职责。实际项目将验收报告存为制品；跨身份清理先设计只读预览与授权，不直接扩大 sudo |

## 推荐顺序与停止点

**先修 C51-01 → 补 C51-03 → 验收一个真实项目 → 再按证据补 API 与交互能力。** 修复涉及 toexec 的共用机制时，在 toexec 独立提交/发依赖 tag，然后显式更新 ccnm；不把产品授权策略塞进共享库，不在本轮修改其他仓库。

真实项目验收至少包含创建/修改、构建、单测、必要的浏览器或端到端测试、审查、提交、制品核对、授权的非生产部署与回退；必须有失败/取消/断线分支。过程和合格标准见[生命周期与职责](../project-lifecycle.md)。需要 SSH 真机、登录、额度或部署时另立具体授权清单；既往 P7/P24 授权不延续到下一轮。

暂不增加新的 Provider、重开封存 exec-server、扩展跨 state 锁服务，或在 ccnm 内做 Planner/Router/任务记忆/发布控制平台。通用执行原语在有消费者和验收用例时再加。

## 本轮文档治理

README 保留中英文简介，其余中文。新增文档导航和生命周期职责矩阵；架构、生产安全、使用/配置、运维/排错、支持矩阵、公开协议入口、开发流程和状态交接同步。旧研究记录及 baseline 保留原日期与计数，不把“文档改过”当产品已修复。

## 本轮验证

已执行：`cargo build --workspace --offline --locked`、`cargo test --workspace --offline --locked`，均退出 0；Rust 935 项（另用 `--list` 核数），格式与严格 Clippy 通过。`python3 -B -m unittest discover -s tests -q`：209 passed，无 skip，文档调整后复跑仍通过。本节故障探针成功复现 C51-01，临时进程及目录已清理。全量测试通过与新探针发现缺陷同时成立，说明原回归没有覆盖该分支。

`check_plan.py`、`check_protocol.py`（38 + 29 个 fixture）及 `git diff --check` 通过；状态记录在 `status.json` 的 P51 evidence 和 `handoff.planning_validation_latest`。P51 完成后 `current_task` 指向 **P52，状态 pending、没有启动实现**。本轮只在上述 macOS 开发机运行，没有重跑 MSRV，也没有新增 Linux、真实 Host/模型、网络出口、已发布产物安装或生产安全验收；不能用历史 P50 的这些证据替代本轮实测。
