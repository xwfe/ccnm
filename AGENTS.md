# ccnm 开发接续入口

开始前依次阅读 [计划维护约定](docs/plan/README.md)、[当前状态](docs/plan/status.json) 和 [实施路线](docs/plan/ROADMAP.md) 中当前阶段。不要把聊天记录、IDE 状态或任何模型私有缓存当成最新实施状态。

## 工作规则

- 不依赖任何特定 MCP、IDE 插件或项目管理工具。至少先运行/查看 `git status --short`、`git log -n 10 --oneline`、当前未暂存和已暂存 diff，再读计划状态；如果客户端自带其他开发工具，可以使用，但它们不能成为接续前提或进度事实来源。
- 状态只以 Git 跟踪的 `docs/plan/status.json` 为准。开始、阻塞、交接、完成都要更新状态；没有证据不能标记完成。
- 默认只完成 `current_task` 对应的一个阶段。用户指定范围优先，但不得跳过依赖和安全门禁。阶段验收后交接，不自动把整份路线连续执行完。
- ccnm 只负责 Agent 的执行机制；Planner、Router、任务图、review/retry 策略和 worktree 编排属于独立 Orchestrator 项目。
- 官方 Agent 的参数、认证、工具策略和输出以实测版本及 fixture 为依据。不得猜参数、复制订阅凭据、读取认证文件内容或实现私有模型客户端。
- 不覆盖、恢复、暂存或提交用户已有修改。禁止 `git add .` / `git add -A`、无关清理和自动 push；按逻辑改动提交本次文件。patch 不匹配时重读并缩小补丁，不用整文件覆盖掩盖失败。
- 不为通过测试重录 golden fixture。修复已知行为缺陷时，单独说明行为变更和新证据，不能伪装成等价重构。
- 文档除 README 开头的中英简介外使用中文；详细设计留在 `docs/`。

## 验证

只改计划：`python3 scripts/check_plan.py`、相关测试和 `git diff --check`。

改 [公开协议](docs/protocol/README.md)（说明、schema 或 fixture）：另跑 `python3 scripts/check_protocol.py` 和 `python3 -m unittest tests.test_check_protocol -q`。

改 `ccnm rpc` 或[黑盒客户端](clients/python/ccnm_machine_client.py)：另跑 `cargo test -p ccnm-core --lib rpc::`、`cargo test -p ccnm-cli --test rpc`，以及 `cargo build` 之后的 `python3 -m unittest tests.test_blackbox_client -q`。

修改 Rust：另跑 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`；Python helper 改动另跑对应 unittest。真机、生产权限和断网验证分别记录，不能用离线测试数量替代。

创建系统账号、ACL、防火墙、独立登录、部署或替换已安装二进制，必须有针对该动作的明确授权。只规划不代表授权执行这些动作。
