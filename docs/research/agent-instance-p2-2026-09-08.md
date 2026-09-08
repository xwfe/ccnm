# P2：Agent Instance 配置模型

本阶段从 `dc5f7db` 的干净工作区开始。先在 `3e43220` 提交[配置契约](../agent-instance-config.md)，再实现模型与关闭门禁；不进入 P3，不部署、不登录、不移动任何真实 profile。

## 已实现

- Runtime workspace 保存 `agent = {node, instance}`，root 只在 Runtime 定义。Agent 本地 `[agents.<id>]` 保存 provider/profile_ref，node 来自 this，避免复制可漂移的定义。
- `instance.rs` 提供引用、公开身份、技术 capability、双端 `WorkspaceBinding` 校验。Agent 验证 instance/provider/profile_ref，Runtime 验证 workspace/root/引用；这是配置绑定，不是 RPC 或网络身份认证。
- `instance/profiles.rs` 私有 registry 不实现 Serialize/Debug。profiles.toml 只在匹配的 Agent 本地读取；位置由 Agent HOME/XDG 决定，不跟随 CCNM_CONFIG。未知 named profile 不退回 default；错误 provider、保留名、重复目录及非法路径拒绝。
- 内建 Codex default 仍是原 `ccnm/agents/codex/`。合成子进程测试另放入错误 CODEX_HOME 和 CCNM_CONFIG 邻接 decoy，确认不被采用；原合成 auth sentinel 保持不变。这不是对真实登录状态的重新验收。
- `configedit::Edit::preview_instance` 在副本中修改指定 workspace；不改 editor 或磁盘，保留其他 workspace、key 的前置注释及 value 的尾注释。自定义 legacy 路径、非默认权限和改 Node 拒绝机械迁移。没有 CLI 自动迁移命令。
- session 可保存公开 agent_identity，必须使用内部版本 3；与 provider/legacy override 冲突会拒绝。旧记录不增加字段，旧 peer 不会丢弃 identity 后执行。旧内部请求拒绝未知身份字段，合法旧 wire/golden 保持。

## 仍关闭的执行路径

新 instance-selected workspace 在现有 Config 执行解析入口明确拒绝，不推断为默认 Claude。实际 Controller、supervisor、session 创建与 Agent transport 不执行 v3 identity 记录；Runtime 也拒绝用旧 MCP payload 操作 instance workspace。CLI 用可执行 marker 脚本验证拒绝发生在 SSH/Agent 拉起之前。

这是必要门禁，而不是已完成的公共 Agent 执行能力。P3 不能只删 guard：必须连接真实 instance/profile 解析、Runtime binding 校验与授权、provider 的认证/启动目录、实际 profile 的输出路径脱敏、公共选择和生命周期；现有 Codex 内部 adapter 仍以原专用 default HOME 运行。显式覆盖 workspace 默认 Agent 的授权与 binding 也属于 P3。

## 验收与证据

| 编号 | 证据 |
| --- | --- |
| P2.1 | 契约、`instance_reference` / `resolve_instance` / `bind_workspace` / 两侧 verify；配对 Runtime/Agent fixture 不重复 root 或 profile 路径 |
| P2.2 | `instance_config` 中同 Node 双 provider、未知/重复/冲突/超长引用、provider/profile 替换、native/hybrid 拒绝；capability 仅指内部 SSH print/interactive 技术支持 |
| P2.3 | legacy 配置/原 golden、注册不改变旧选择、只读且幂等的预览、注释保留、Agent-local default 路径/合成 auth sentinel 保持、symlink 与不安全 profile 文件拒绝 |
| P2.4 | 配置往返与 `binding.json`，公开 binding 不含目录；v3 session 版本/身份校验；`instance_closed` 验证公共和旧内部入口均不会把新 instance 当 legacy 执行 |

新增 fixture 在 `tests/fixtures/agent-instance/`，全部为合成数据，不是官方 CLI 测量或生产机器配置。Runtime 的未知远端 instance 在 Agent 解析时拒绝，而不是由 Runtime 猜测注册表；只有 Agent 本地上下文可以给纯解析 API 提供 home/xdg。

```bash
cargo test -p ccnm-core --test instance_config
cargo test -p ccnm-cli --test instance_closed
cargo test -p ccnm-core --test provider_compat
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -B -m unittest discover -s tests -p 'test_*.py' -v
python3 scripts/check_plan.py
git diff --check
```

本轮 macOS 最终门禁：**485 Rust passed / 0 failed，19 Python passed**；fmt、严格 clippy、计划检查通过。P2 专项为 19 个模型/绑定测试与 3 个 CLI 关闭门禁测试；没有运行真实 Agent 或生产权限/网络验收，未重录原 Claude/Codex golden。

过程中的失败也保留边界：新增语法的两个测试在实现前失败；预览丢失 key 前置注释的回归先失败，随后修复 key/value decor。第一轮全量中的既有 `mcp::patch::tests::a_journal_held_by_a_running_commit_is_left_alone` 曾失败，定点复测及后续完整并行门禁通过；本轮未修改 patch 模块，不能声称该非稳定现象已修复。

全局拒绝 session 未知字段的尝试也被既有 CLI 回归拦住：含 runtime_alias/runtime_ccnm_bin 等历史字段的旧结果应继续可读。最终未对旧存储记录施加该限制；新身份字段自身严格解析，旧调用请求严格拒绝夹带未知身份，v3 记录靠版本及关闭门禁防止误执行。没有改写旧 fixture 来绕过这项回归。

本阶段分契约/认领与实现/证据两个提交。配置解析与关闭门禁必须一同交付，避免仅增加 agent 字段后落到 legacy Claude。下一项是 P3；其真实执行身份、ACL、sudo/socket、部署和登录操作仍须明确授权。
