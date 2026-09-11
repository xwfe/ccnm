# ccnm 开发接续入口

开始前依次阅读 [计划维护约定](docs/plan/README.md)、[当前状态](docs/plan/status.json) 和 [实施路线](docs/plan/ROADMAP.md) 中当前阶段。不要把聊天记录、IDE 状态或任何模型私有缓存当成最新实施状态。

## 工作规则

- 不依赖任何特定 MCP、IDE 插件或项目管理工具。至少先运行/查看 `git status --short`、`git log -n 10 --oneline`、当前未暂存和已暂存 diff，再读计划状态；如果客户端自带其他开发工具，可以使用，但它们不能成为接续前提或进度事实来源。
- 状态只以 Git 跟踪的 `docs/plan/status.json` 为准。开始、阻塞、交接、完成都要更新状态；没有证据不能标记完成。
- 默认只完成 `current_task` 对应的一个阶段。用户指定范围优先，但不得跳过依赖和安全门禁。阶段验收后交接，不自动把整份路线连续执行完。
- ccnm 只负责 Agent 的执行机制；Planner、Router、任务图、review/retry 策略和 worktree 编排属于独立 Orchestrator 项目。
- ccnm 有两个执行入口，边界见 [双执行入口方案](docs/plan/runtime-surfaces.md)：v1 主线是 Managed Agent Runtime（`runtime → agent → runtime`）；v1.x 扩展是外部 MCP client → ccnm → remote Runtime。两者共用 Runtime 安全/工具/写互斥，不把 Remote MCP 做成裸 SSH，也不实现 Agent 凭据代理。
- Runtime Executor（通常是 `ccrun`）是入站执行身份，不应持有 ccnm 正常运行所需的主动 SSH 私钥/SSH agent。public CLI/RPC 的 Operator、Agent 登录身份和 Runtime Executor 不能再视为同一个 OS identity；P7 冻结前先修这个边界。
- 官方 Agent 的参数、认证、工具策略和输出以实测版本及 fixture 为依据。不得猜参数、复制订阅凭据、读取认证文件内容或实现私有模型客户端。
- 不覆盖、恢复、暂存或提交用户已有修改。禁止 `git add .` / `git add -A`、无关清理和自动 push；按逻辑改动提交本次文件。patch 不匹配时重读并缩小补丁，不用整文件覆盖掩盖失败。
- 不为通过测试重录 golden fixture。修复已知行为缺陷时，单独说明行为变更和新证据，不能伪装成等价重构。
- 文档除 README 开头的中英简介外使用中文；详细设计留在 `docs/`。

## 验证

只改计划：`python3 scripts/check_plan.py`、相关测试和 `git diff --check`。

改 [公开协议](docs/protocol/README.md)（两套契约的说明、schema 或 fixture）：另跑 `python3 scripts/check_protocol.py` 和 `python3 -m unittest tests.test_check_protocol -q`。[Remote Workspace MCP](docs/protocol/remote-workspace-mcp-v1.md) 还没有实现，改它只动文档与 fixture；但凡文中写"实测"的地方，依据必须是仓库代码或 SDK 源码，不能是记忆。

改 `ccnm rpc` 或[黑盒客户端](clients/python/ccnm_machine_client.py)：另跑 `cargo test -p ccnm-core --lib rpc::`、`cargo test -p ccnm-cli --test rpc`，以及 `cargo build` 之后的 `python3 -m unittest tests.test_blackbox_client -q`。

改[执行接口示例](clients/python/execution_backend.py)或[交接文档](docs/orchestrator-handoff.md)：另跑 `cargo build` 之后的 `python3 -m unittest tests.test_execution_backend -q`。它是给独立 Orchestrator 抄走的示例，不是 ccnm 的运行路径；改它不影响 `ccnm rpc` 的行为，但两个 Python 文件都必须能脱离仓库单独跑。

改 Runtime 权威解析（`crates/ccnm-core/src/runtime.rs`，internal wire protocol 4）：另跑 `cargo test -p ccnm-core --lib runtime::` 和 `cargo test -p ccnm-cli --test runtime_open`。前者证明决策本身，后者证明真实二进制的 `internal mcp-serve` 确实按 protocol 数字分派、且不认识的版本会停下而不是回退。

改 [P7.3 对照工具](scripts/p7_parity_check.py)：另跑 `cargo build` 之后的 `python3 -m unittest tests.test_p7_parity -q`。它自己的成功路径由 `tests/fixtures/fake_ccnm.py` 离线覆盖，真机结论仍以 `--out` 写出的证据文件为准。

修改 Rust：另跑 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；Python helper 改动另跑对应 unittest。真机、生产权限和断网验证分别记录，不能用离线测试数量替代。

创建系统账号、ACL、防火墙、独立登录、部署或替换已安装二进制，必须有针对该动作的明确授权。只规划不代表授权执行这些动作。
