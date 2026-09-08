# P3 Codex 公共链路实测

## 调用分工与配置

沿用既有分工，不改 Runtime/MCP：fodelf 用户从 Runtime Node 发起公开 CLI，使用已有 xdwmbp alias 调用本机 Agent；本机 Agent 的独立 alias `ccnm-p3-runtime-r7enw5` 只以 UID/GID550 连接 Runtime MCP。没有给 Runtime 执行账号放入私钥，也没有复制既有私钥或转发 SSH agent。

本机 `/tmp/ccnm-p3-agent-m8wmi9ng` 保存独立 Agent config/state 和 tmux 临时目录，wrapper 固定这些路径；官方 Codex 仍使用已有 ccnm 专用 HOME，认证文件未读取或复制。新增本机 SSH alias 前保存原配置私有备份，只前置本轮独有 block，不改原 alias。临时 Controller 使用 internal controller 在实际 Aqua 上下文启动，不安装 LaunchAgent或替换原服务。

远端普通用户控制配置在 `/tmp/ccnm-p3-setup.TnaRle/runtime-control.toml`，指向相同 Runtime workspace。仅将测试父目录改为0711和公共构建改为0755，使用户 CLI 可验证目录存在并运行构建；内部 project/state/config 仍私有，模型文件操作始终走低权限 MCP。Agent 的 Runtime wrapper 固定 MCP 的原配置与真实 HOME；没有放宽 unconfined。

## 已验证

证据见 `tests/fixtures/p3-public-codex/`。

1. 公共 `mcp probe p3check --agent codex-main --calls 3` 返回7工具，同一服务 PID41991，初始化约600ms。
2. 公共 `run p3check --agent codex-main --timeout 120 --print <只读测试提示>`：官方0.153.4退出0，结果 P3_PUBLIC_CODEX_OK；官方JSONL实际完成 read_file、workspace_info、exec_command，不以模型自述替代工具记录。精确status为Completed，重复stop未发送signal。
3. 公共 `run --detached` 启动 interactive，精确attach成功。首次工具PTY的TERM不足，tmux报不支持clear；仅为测试终端设置 TERM=xterm-256color 后成功，不修改产品。官方CLI对本轮空临时cwd提示信任，先记录受影响project entry，再确认；之后该entry已精确移除，未覆盖其他配置。
4. interactive实际调用 workspace_info，返回 P3_INTERACTIVE_READY。C-b d分离后仍Running/tools connected。仅终止本轮Controller（核对监听socket），在同一配置下重启，PID52008变57619，两次Aqua；同一个session继续Running且可精确reattach。
5. Ctrl-D后附着终端退出0，session为Completed；Runtime PID43752已消失，guard为released。
6. 另起无提示的空闲interactive，不额外请求模型。按其Agent子进程树和解码payload中的精确session核对SSH PID59006后发送TERM；status正确区分Agent仍Running与TOOLS DOWN。精确stop成功，session记Failed（受控终止），guard released。这是transport进程故障，不称为物理断网验收，也不把attach当resume。

官方CLI提示额度接近上限；没有更换模型、消耗reset或修改提醒设置。只完成上述两个模型提示，其余为空闲会话/进程控制。

## 清理与接续

本轮三个session的已记录PID/进程组均无残留；临时Controller已停止。临时Codex项目trust entry恢复，官方登录与官方会话历史保留。SSH alias/备份、临时配置、测试项目/日志和系统账号/组仍按清单保留，不能宣称最终零遗留。

下一步是反向Claude公共链路：fodelf Agent已生成新的本轮SSH传输key，私钥只在fodelf；本机仅收到公钥，指纹 `SHA256:R/ybKNAaN8JQxYETPhpbfeYmh199zOunKmNFT8mP54M`。需要用户在本机管理员终端运行 `scripts/p3-authorize-local-runtime.sh --apply`，仅追加到已有ccrun，备份已有公开authorized_keys，不改UID/主组或复制凭据。该脚本尚未执行；入口单测不代表真实授权成功。

本轮无Rust改动，Python26、计划检查通过；Rust514为上一轮历史结果。P3仍未完成，Claude方向、完整提权/egress以及最终清理尚缺验收；不进入P4。
