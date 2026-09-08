# P3 专用 Runtime 实测

## 构建与传输

源码 `08c23ac`，本机 `cargo build --release -p ccnm-cli` 通过。当前构建 9,732,640 bytes，SHA-256 `8bf2efe21ec4281d1ff0b95b00cee64625cb5e351c28280a4489fdd372462bcc`。仅部署 `/Users/Shared/ccnm-p3-runtime.R7Enw5`，账号 UID/GID550，真实 HOME `/Users/ccnmp3test`；没有替换已安装二进制、Controller 或 Claude 默认配置。

SSH 恢复可达后，32 KiB 单块在3.52秒内通过校验。4路并发出现超时，检查已落盘16块均正确；单独补缺块2.76秒通过。改串行后又有一次超时，重新获取每块哈希并跳过35块已确认产物，最终101块全部校验通过。分块只创建唯一名称，不向已有块续写；完整 gzip 与解压后二进制另行核验 SHA-256 后才运行。该过程证明构建完整到达，不证明网络根因或长期稳定性已解决。

## 真实 MCP：七工具与编译执行

使用独立 Runtime 配置，`this=runtime`、`runtime_user=ccnmp3test`、workspace 绑定 `agent/codex-main`、`allow_unconfined_exec=false`。v3 payload 带完整 identity，只有 provider/profile 的公开名称，无 CODEX_HOME、profile路径或凭据；实际 HOME 不替换。

`tests/fixtures/p3-isolated-runtime/mcp-seven-tools.json` 保存实际请求/响应及配置：

- initialize 成功，tools/list 返回7工具。
- workspace_info 指向 Runtime 测试 Git 项目。
- apply_patch 新建 smoke.c，read_file/list_files/search_text 复核。
- exec_command 通过 Runtime `/usr/bin/cc` 编译，再运行产物打印 `P3_RUNTIME_OK`；read_output 读取对应输出。
- exec_command 运行 Runtime Git，确认新增文件。
- stdin EOF 后 SSH exit0；另起连接确认 server PID37511 不存在，Runtime guard 内容为 `released`。

第一次探针已经编译运行成功，但测试脚本误把 MCP 文本结果当 JSON，导致 read_output 前解析失败。保留该事实，没有修改产品来迎合探针。修正为根据实际文本提取 output_ref；确认旧 server 已退出、只删除本轮两个项目文件后，以新 session 从干净项目文件基线重新跑完七工具。不是通过覆盖 fixture 隐藏产品回归。

本机官方 CLI 再测为 Codex 0.153.4，ccnm 专用 HOME 的官方 login status 表示 ChatGPT 已登录；tmux 3.7c，当前登录上下文 Aqua。不读取认证文件内容。本轮尚未启动任何模型，七工具结果不能替代公共 Agent 生命周期验收。

## 公共入口待接续

在专用账号执行公开 doctor，Runtime 身份与执行安全检查通过；首次报告 Runtime ccnm 默认路径不匹配和 Agent SSH host key verification failed。已将测试配置的 nodes.runtime.ccnm_bin 指向独立 wrapper，固定该测试 config/state，保留真实 HOME；不安装到默认路径。

双向公共控制链尚未配置完。`xdwmbp` 是原 fodelf 用户环境的 alias，不能假定临时 Runtime 用户继承该用户的 SSH 信任或身份。下一步核对既有“Runtime 用户发起 CLI / 专用身份承载 MCP”的调用分工及控制路径，再接入临时 Agent Controller，禁止为连通复制现有私钥、转发 SSH agent、绕过 host key 校验或放宽凭据门禁。

## 清理和限制

101个传输块、gzip、transport-probe 已按明确清单删除。当前保留 Runtime 测试目录中的 ccnm、ccnm-runtime、config.toml、project 与 state 供接续；用户账号/组、公钥、本机临时key、准备脚本与 root 清单仍待最终清理，详见系统准备文档。用户服务域可能再次有 distnoted，最终清理需核对，不能只删除账号文件。

P3 尚未完成；真实官方 Agent 的 print/interactive、stop/断线/Controller重启、双 Provider及完整 egress/提权审计仍缺验收。本轮没有 Rust 修改；release构建、计划检查和Python25通过，上一轮Rust514是历史门禁结果。
