# P3 授权双机预检

用户已批准本机与 fodelf 互换 Agent/Runtime 角色、独立临时部署、真实官方 Agent 验证与只读安全审计；不替换现有服务、不修改系统账号/ACL/防火墙，完成后清理本轮资源。本记录是实机预检，不是双 Provider 公共链路通过。

## 实测

脱敏结果见 `tests/fixtures/p3-node-preflight/result.json`。

- 本机 Codex `0.153.4`，以 ccnm 专用 HOME 运行官方 `codex login status`，返回 ChatGPT 已登录；不读取 auth 文件。
- fodelf SSH PATH 找不到 Claude；固定已知路径 `~/.local/bin/claude` 返回 `2.1.263`。官方 `auth status --json` 只保留退出码与 loggedIn 布尔值，后者为 true。SSH 上下文为 Background，不当作登录 Controller。
- `id` 证明两端当前 SSH/本地账号属于 admin。本机 `id ccrun` 成功，但 `sudo -n -u ccrun /usr/bin/id` 要求密码；fodelf 没有 ccrun。fodelf 的 `ssh -G xdwmbp` 解析到本机开发者账号，而非 Runtime 专用身份。
- 仅用 `test -r` 检查 fodelf 已知 Claude 凭据与 login Keychain 路径，均可读；不打开内容。Docker socket 可写；`sudo -n -l` 不可用，这不证明所有提权路径都已禁止。
- 本机当前构建（`cargo build -p ccnm-cli`）在独立临时 workspace/config/state、真实 HOME 下运行 `internal mcp-serve`。第一次手工 payload 缺少 policy，退出 11；补齐 `policy=coding` 后退出 33，stdout 为空，报告已知 Agent 凭据可访问。设置 unconfined opt-in 仍拒绝，未改项目。

## 判断与清理

不能以更换 HOME 隐藏当前 UID 可读凭据来重复历史 scratch 成功路径；那不是 OS 隔离。当前入口未通过非豁免门禁，所以没有启动真实模型任务，也没有上传二进制或拉起临时 Controller。

两次本机临时目录都由 TemporaryDirectory 退出清理，修正后的清理检查为 true。远端只执行只读命令，没有创建目录、部署或后台进程。保留用户既有登录、Controller、tmux 和历史资源；不能把不属于本轮的 session 一概清除。

P3.5 现在缺的是可用且受限的 Runtime 身份入口，而非本次验证授权。需另行批准系统准备：使两端专用账号具备仅测试 workspace/toolchain 的访问权、从 Agent 通过 OpenSSH alias 登录，核对敏感资源和 egress。账号/ACL/SSH 配置及网络策略变更必须先列出差异和恢复方案；不能把本次只读审计授权扩展成系统变更授权。
