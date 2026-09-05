# 使用说明

`ccnm doctor <workspace>` 的基础链路确认正常后，同一个 workspace 可以从 Runtime Node 或 Agent Node 发起。

## 交互式会话

```bash
ccnm my-project
# 等价完整写法
ccnm run my-project
```

Claude session 本身运行在 Agent Node；它对项目的读取、搜索、修改和命令执行通过 MCP 落到 Runtime Node。

只启动、不 attach：

```bash
ccnm my-project --detached
```

之后重新接入：

```bash
ccnm attach my-project
```

SSH 断开、终端关闭或笔记本暂时离线，不等于结束 session。只要 Agent Node 上的 tmux/session 还活着，就可以重新 attach。

## Prompt

单行开场白：

```bash
ccnm my-project "修复 parser 测试失败"
```

包含多行、引号或其他不适合放进远端 shell argv 的内容时，用 stdin：

```bash
ccnm my-project --prompt-stdin <<'EOF'
重构 parser。
保持 "strict" 行为不变。
先跑聚焦测试，再跑完整测试。
EOF
```

自由文本不会拼进远端 SSH 命令行，而是通过 stdin 传递，避免被远端 shell 重新解析。

## 查看状态和结束会话

```bash
ccnm status my-project
ccnm status my-project --all
ccnm stop my-project
```

session 建立后，这些操作属于 Agent 侧 session 管理，不需要重新解析 workspace root。

## 非交互 `--print`

当前应在定义 workspace 的一侧执行，通常就是 Runtime Node：

```bash
ccnm run my-project --print "找出问题，修复，然后运行测试"
```

如果执行时 SSH 断开，完成后的结果仍然保存在 Agent Node。读取最近一次结果：

```bash
ccnm result my-project
```

也可以指定 session id：

```bash
ccnm result my-project --session <id>
```

## MCP 诊断

本地 Runtime 诊断：

```bash
ccnm mcp probe my-project --local --calls 100
```

它会启动一个真实 `ccnm internal mcp-serve` 子进程，证明多次 MCP 调用由同一个持久 runtime process 处理，而不是每个工具调用都重新启动一次进程。

真实跨 Node 链路由：

```bash
ccnm doctor my-project
```

进行验证。

## 当前模型能做什么

核心 MCP 工具：

```text
workspace_info
read_file
list_files
search_text
apply_patch
exec_command
read_output
```

主要行为：

- `read_file`、`list_files`、`search_text` 都受 workspace 路径边界约束；
- `apply_patch` 是结构化写入路径，带版本检查，并提供事务/恢复保护；
- `exec_command` 使用 argv，不主动通过 shell 执行，但调用者仍然可以显式运行 `sh -c` 等程序，所以它本质上仍然是命令执行能力；
- 大输出由 `read_output` 分页读取，避免一次把全部输出塞进模型上下文；
- 项目根 `CLAUDE.md` 会投影到会话，其余规则文件按路径提示，模型按需读取；
- 受管理的 Claude session 会禁用原生 Read/Edit/Write/Grep/Glob/Bash，让项目访问统一走 Runtime Node。

## 当前不做什么

以下能力暂时延后，不按功能清单机械实现：

- Git 专用 MCP 工具；
- 托管后台长进程；
- Browser provider；
- image provider；
- Linux Controller；
- 多 Agent 自动编排。

优先让真实项目 dogfood 暴露真正高频、浪费 token 或需要人工介入的缺口，再定义这些工具的契约。
