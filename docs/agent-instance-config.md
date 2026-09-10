# Agent Instance 配置契约（P2）

状态与验收见 `docs/plan/status.json`。P2 只交付配置、身份绑定与只读预览；公开执行入口在 P3 才开放。下述新配置即使合法，也不能通过旧入口退回 Claude 执行。

> 这是 P2 停止点的历史契约；当前 P3 已接入公共执行。运行语义和当前验收级别见[单 Agent 执行契约](agent-execution-p3.md)与[支持矩阵](support-matrix.md)，不要把本页的 P2 阶段描述当成当前开关状态。

## 唯一事实来源

- Runtime 的 `[workspaces.<name>]` 唯一定义 root、runtime_node 与 Agent 引用。新引用为 `agent = { node = "worker", instance = "claude-main" }`，不复制 provider、profile 或第二份 root。
- Agent 的本机配置定义 `[agents.<instance>] provider = "claude" | "codex"`、`profile_ref = "default" | <name>`，Codex 还可以加一个可选的 `model`。所属 node 隐含为这份配置的 `this`；instance 名只在该 node 内唯一，不需要全局目录服务。Runtime 只持 node/instance 引用，不复制此表。

  ```toml
  [agents.codex-main]
  provider = "codex"
  profile_ref = "default"
  model = "gpt-5.3-codex-spark"   # 可选，仅 Codex
  ```

  **`model` 只有 Codex 能写**，给 Claude instance 写会被配置校验拒绝——ccnm 不给 Claude 传模型，它的模型是 Claude 自己配置的事。之所以需要这个字段：ccnm 用 `--ignore-user-config` 启动 Codex（刻意的，免得 Agent 上的一个文件改掉实测过的行为），于是 CLI 配置文件里的 `model` 不生效，不给这个字段就**根本没有办法选模型**。不写就用 CLI 自己的默认值，也就是所有 fixture 当初被测量时用的那个。

  它和 `provider`/profile 一样是 **Agent 本机的事实**：不进 `AgentIdentity`、不进 binding、不进会话记录，也不上任何 wire。supervisor 启动前会重新读一次本机 registry 解析 profile 目录，模型在同一处一起取。
- 私有目录只放 Agent 本地的 `$XDG_CONFIG_HOME/ccnm/profiles.toml`（默认 `~/.config/ccnm/profiles.toml`）。`[profiles.<name>]` 定义 provider 与绝对 directory；不从 `CCNM_CONFIG` 或远端消息指定该文件位置。该文件不存 token，也不随共享配置、binding 或 session identity 序列化。
- 内建 `default` 按 provider 区分：Claude 使用官方默认目录；Codex 使用现有 `~/.config/ccnm/agents/codex/`，尊重 Agent 的 XDG_CONFIG_HOME。不能覆盖 default，不能迁移、复制或链接现有 auth。

选择结果为 `{node, instance, provider, profile_ref}`；只有可公开的引用。Runtime 冻结 workspace/root/runtime_node 与这份身份形成 binding。Agent 重新从自己 registry/profile 解析并比较全部身份字段，拒绝错节点、未知引用、provider/profile 变更；Runtime 用本地 workspace 重算并比较 binding，拒绝调用方覆盖 root 或引用。Agent 不为验证 root 复制一份 workspace 配置。

这些是库层的数据契约，不是新增 RPC 协议或远端鉴权。P3 的执行入口必须使用双端检查；P2 不接受新的可执行 wire 请求。

## 校验与兼容

- 新配置/身份/引用拒绝未知字段、未知 provider、重复 TOML 定义和非法名字。新引用的 node/instance/profile 及 instance workspace 名为最多 64 字节的 `[A-Za-z0-9][A-Za-z0-9_-]*`，不能用路径代替引用；不回改 legacy 名字规则。
- workspace 必须且只能使用 `agent_node`（legacy）或 `agent`（instance）之一。新引用不能同时使用非默认 `claude_permission_mode`，也不能借 Node 的 `claude_config_dir` 偷换 profile；有旧自定义目录时先处理迁移冲突。
- Agent registry 只定义本机 instance；profile 引用在 Agent 解析时校验，Runtime 不猜远端是否存在。普通配置 parse 不读取 profiles 文件、认证文件或目录。
- 新 instance workspace 的 root 只能出现在它的 Runtime 配置中。Agent 接收 binding 时核对 Runtime 是否在本地 nodes 中可达；顶层 runtime_node 只是 CLI 默认委派目标，不是第二份 workspace 定义或唯一 Runtime 授权名单。
- 已知内部支持是 SSH MCP + print/interactive，不是生产 READY。两 Provider 的新 instance colocated/native 均拒绝：Claude 有未修复缺陷，Codex 没有验收。hybrid 等未实现 backend 也拒绝。旧 Claude topology/参数行为不顺手修复。
- 没有新字段的配置继续原样解析/执行。instance-selected workspace 在现有执行解析入口明确报未开放，不能误用默认 Claude；注册 instance 本身不改变旧 workspace 的选择。
- session 增加可选的公开 `agent_identity`；旧记录不增加字段。含 identity 的记录要求内部版本 3，旧 peer 拒绝而非忽略身份后执行；P2 所有实际 session 创建/启动/supervise 入口仍拒绝这种记录。身份不一致、损坏或过期绑定拒绝。
- 旧 run/start 请求拒绝夹带新身份字段；Runtime 的旧 MCP payload 也不能执行已选择 instance 的 workspace。P3 必须接入真实 binding 校验，不能只删除关闭开关。

## Profile 与迁移

配置解析只返回计划，不表示目录存在、已登录或安全。执行前仍须 P1 的属主/权限/symlink 检查和官方 CLI 认证/版本探测；新 profile 必须由用户在 Agent 登录会话里独立官方登录。没有真实登录的 profile 不能自动借用 default。

named profile 拒绝相同目录字面路径和覆盖内建 default；文件系统上的其他别名/ACL、实际登录与运行时目录注入仍需 P3 验证。profiles.toml 自身要求当前 UID 所有、仅属主可读写、非 symlink；文件缺失只提供内建 default，不让未知 named ref 回落。

迁移预览是 `configedit` 的只读 API：在内存里的 TOML 副本中，把指定 legacy workspace 改成同一 node 的 instance 引用，保留其他 workspace 和注释；返回候选文本，不调用 save。非默认权限、自定义 legacy 目录或跨 node 替换拒绝自动转换，避免把安全含义不同的字段机械搬过去。Agent registry/profile 必须另在 Agent 本地准备并验证；预览不伪造它们，也不复制私有目录。

P2 不提供迁移写入命令、不自动改用户配置。新语法与预览 API 的离线 fixture 会验证可用性；新 instance 的真实 launch/doctor/status 闭环留在 P3。
