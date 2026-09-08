# P1：Provider 安全契约收敛

对应 `docs/plan/ROADMAP.md` 的 P1，仅离线实现与验证；没有部署、登录、创建账号或改 ACL/网络。契约先于实现提交于 `471c28f`，正文见 [Provider 安全契约](../provider-safety.md)。

## 根因与明确行为变更

先补两个失败测试：默认 Claude 审计漏掉 `.codex/auth.json`；Claude 预检 SSH 没有 `ForwardAgent=no` 和完整环境移除。两项在修改前都确实失败，不是根据代码猜测。

进一步跟踪发现 Claude 的 MCP JSON 只保存 command/args，不会执行 Rust `Cmd.env_remove`。只在 `Ssh` 构造器里标记要删除变量，并不能保护 CLI 实际拉起的 MCP transport。因此：

- Provider 仅声明凭据路径/前缀/容器及已测需求；共享 `safety/environment.rs`、`safety/credentials.rs` 落实策略。
- 原 Codex transport 移到 `session/transport.rs`，保留薄兼容出口。两 CLI 都先启动 Agent-local Rust wrapper，再 exec 固定 `/usr/bin/ssh`；预检 SSH 使用同一环境规则。`Cmd::process` 共用于受控 spawn 和 transport exec，避免两套环境应用代码漂移。
- 新连接禁用 forwarding/连接复用，新增受控 `SetEnv`；已有 master 的手动查询/停止仍可用。可能依赖原连接复用的性能会变化，不声称是字节等价改动。
- Runtime 检查所有已知 Provider，不按当前 provider 过滤；可访问性使用 metadata + 本机 `/bin/test -r`，不读取认证文件/私钥内容。Agent 私有目录校验用有效 UID，不再拿 HOME 的属主代替进程身份。
- 未知身份、可访问/未知 Agent 凭据和未授权认证环境不可被 `allow_unconfined_exec` 跳过。MCP 初始化在 Git 探测前拒绝；exec 前再次检查，doctor 使用同一判断。`id`/`sudo` 诊断固定系统路径，探测异常不当作权限已拒绝。
- 已知公开 SSH 文件（包括 `authorized_keys`）不当私钥；其他候选保守处理。DNS/连接探测失败只报告不确定，不证明网络已隔离。

这轮不改官方 Claude CLI 参数、工具策略、默认 provider、公开字段或 topology，也不修 colocated 已知缺陷。原 Claude golden 文件及 Codex 真机 fixture 未重录。兼容测试仅给原 golden 中的 **直接 SSH 编码**附加明确列出的安全选项差异；产品生成的 **wrapper MCP JSON**另由 session/launcher 测试断言，不能用前者代替后者。

## 每一层的证据

| 验收 | 单独证据 | 不代表什么 |
| --- | --- | --- |
| P1.1 | `docs/provider-safety.md` 定义来源、身份、网络、未知与 native 边界；本记录列出行为变化 | 不代表已配置 OS 隔离 |
| P1.2 / P1.3 | `safety::tests::codex_credentials_are_checked_even_when_claude_is_the_default` 先红后绿；`safety::provider_tests` 检查两 provider、默认/专用/本地引用/Keychain 容器、访问拒绝/未知、坏引用和隐私输出 | 只检查已知位置，不证明机器没有其他秘密 |
| P1.2 / P1.3 | `ssh::tests::preflight_and_transport_isolate_both_providers` 检查每种 provider 的预检与 transport 命令；`provider_safety` 子进程重入测试把合成认证放进继承环境，使用产品共用命令/环境执行函数，仅把 SSH 换成测试程序；实际子进程记录变量名与普通 marker | 不运行官方 Agent；不连接另一台机器 |
| P1.3 | `private_home_binds_effective_uid_not_home_directory_owner` 等覆盖错误 UID、共享权限、symlink、非文件、缺失和失败探测；CLI supervisor 用可执行 marker 脚本证明坏 HOME 不启动 Agent | 错 UID 通过合成身份模拟，不创建真实系统账号 |
| P1.3 | `provider_authentication_environment_cannot_be_waived_or_leak` 对两 provider 各测试四种认证环境；即使 unconfined=true，MCP 拒绝初始化，PATH 中测试 Git 的 marker 不产生，输出不含合成秘密值 | 受控的 id/sudo/metadata 诊断仍会执行 |
| P1.3 | `credential_added_after_handshake_still_refuses_child_before_spawn` 对两 provider 的正规 auth 文件和 dangling symlink，在握手后新增，exec 拒绝且 marker 不产生 | OS 权限仍需阻止检查后的权限变化，不是无竞态 sandbox |
| P1.3 / P1.4 | `runtime_child_keeps_project_environment_but_not_provider_private_metadata` 实际 MCP 子进程保留普通项目变量，删除 Claude/Codex 私有 metadata；未借用 Runtime `env -i` 作为上游清理证据 | 不增加项目秘密授权 API，不保证任意项目输出无秘密 |
| P1.4 | 原 Claude CLI/策略/wire golden、Codex pin/JSONL/策略 fixture 和全量 Rust/Python 门禁 | 不是重跑 P0 真机或 P3 生产验收 |

OpenSSH 专项实测在 `tests/fixtures/provider-safety/ssh-options.json`：使用当前系统 `ssh -G -F` 与新建的合成配置，不读取个人 SSH 配置、不连接网络。`SetEnv=none` 实际 exit 255；常量 `SetEnv=CCNM_TRANSPORT=1` 覆盖配置字面值。`SendEnv=-*` 后仍可能看到配置的 OPENAI_API_KEY 名称，故“选项看起来清理了”不能证明变量值被删除。测试另验证实际命令环境中该变量不存在。

## 重跑与结果

```bash
cargo test -p ccnm-core safety
cargo test -p ccnm-cli --test provider_safety
cargo test -p ccnm-cli --test mcp_read_file
cargo test -p ccnm-core --test provider_compat
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -B -m unittest discover -s tests -p 'test_*.py' -v
python3 scripts/check_plan.py
git diff --check
```

macOS 本轮门禁：Rust **463 passed / 0 failed**；Python **19 passed**（原 Codex helper 7 项 + 计划检查 12 项）。测试使用合成 HOME/配置和凭据，不读取真实认证内容、不消耗 Agent 订阅；OpenSSH 只运行本机配置解析。基线 448 Rust / 7 Python 仍是 P0 历史记录，不改写历史测试数。

## 下一阶段与未覆盖

P2 才开始 Agent Instance 公共配置模型；Codex 公开入口继续关闭。P3 需要明确授权后验证真实执行身份、ACL、sudo/socket、服务端环境来源、egress 及两 Provider 公共链路。P1 没有更新两端安装，也没有证明旧 Runtime 具备新检查；现有同为 `0.2.0` 的 v1/v2 握手不是 P1 安全能力协商，部署时须核对两端构建来源/指纹。任意其他目录和 credential service 仍不能由本次有界检查排除。

为保持每个提交可回滚，本阶段分为契约/认领与实现/证据两个逻辑提交；后者必须一起更新 MCP wrapper、状态识别、共享安全检查及其兼容回归，不能只提交半条清理链路。
