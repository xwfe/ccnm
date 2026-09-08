# P3 双机 Runtime 系统准备

这是 P3.5 的环境准备，不是 P4 或新功能。验证与角色互换已获授权；用户随后同意系统准备。当前两端 `sudo -n true` 均返回需要密码，所以尚未执行任何特权变更。批准执行不等于现有进程拥有管理员权限；密码只能输入两端操作系统的认证界面，不发送给 Agent。

## 对象、影响与最小范围

| 对象 | 准备动作 | 保持不变 |
| --- | --- | --- |
| 本机 Runtime | 复用已有 `ccrun`（UID 504，home `/Users/ccrun`）；先核对现有授权，再追加测试公钥 | 不删除/重建该账号，不修改其 UID、shell 或原有授权 |
| fodelf Runtime | 创建普通专用 Runtime 账号；创建前重新检查账号名与 UID，发现同名则停止核对 | 不加入 admin，不赋予 sudo，不沿用 Agent 个人登录身份 |
| 两端测试目录 | 各建唯一 `/Users/Shared/ccnm-p3-<run-id>`，内部 workspace/state 属于 Runtime 账号，默认 0700；ccnm 测试构建独立存放 | 不向个人 home 添加穿透 ACL，不授权真实项目，不修改已安装 ccnm |
| SSH 公钥认证 | 每个 Agent 节点本机生成一次性测试密钥；只将公钥追加至另一端 Runtime 的 authorized_keys；禁止 agent/端口转发 | 私钥只留生成它的 Agent，不读取或复制现有密钥；不接触 Claude/Codex auth 文件 |
| Agent SSH 配置 | 私有备份后追加唯一测试 alias；目标地址/端口取已有 alias 的实际配置，User 指向专用 Runtime，明确指定测试 key/IdentitiesOnly/ForwardAgent=no | 原 `fodelf`、`xdwmbp` alias 原样保留；不假定更换 User 就一定能通过现有 SSH 服务 |
| 临时 Controller/tmux | 独立 state/socket，位于对应 Agent 的正常登录上下文 | 不重启或替换现有 Controller，不操作默认 tmux socket |
| 网络 | 先只读审计，记录实际可达性和策略来源 | 不自动调整防火墙，不把一次不可达称为已隔离；若需新增规则，先明确规则与回滚并单独确认 |

测试公钥需禁止转发；有交互终端需求的控制通道与纯 stdio MCP 通道不能机械共用一个禁止 PTY 的限制。若现有 SSH 服务拒绝专用账号，停止并检查该服务的用户策略，不修改或绕过网络产品权限以求连通。

## 执行顺序

1. 管理员在两端本机登录终端确认账号和 SSH 服务配置；本机现有 ccrun 的 authorized_keys/ACL 只检查元数据和必要公开授权，不读取其他私有状态。
2. 生成此次唯一资源清单，再执行上表的账号/目录/公钥/alias 准备。个人配置修改前私有备份；已有 authorized_keys 只追加带唯一标记的公钥行，不整体覆盖。
3. 两端分别通过新增 OpenSSH alias 执行 `id`，确认真实 UID。仅本机 sudo 切换成功不等于 SSH 链路验收成功。
4. 以同一个 Runtime 身份验证测试项目读写、Git/构建/测试和依赖；只读检查对 Agent home、已知两 Provider 凭据、SSH 私有状态与 Docker socket 的访问均被拒绝，验证无 sudo/admin。禁止用假 HOME 隐藏凭据。
5. 对同一测试构建分别运行 Claude→本机 Runtime、Codex→fodelf Runtime 公共入口；完成 print/interactive/精确 session、Ctrl-D/stop、detach/reattach、Controller 重启与链路失败，保存脱敏 fixture。
6. 完成 cleanup 后才记录 P3.5 结果；任一身份/网络/生命周期门禁未通过则保持未完成，不以 scratch 回归替代。

## 回滚与清理

按资源清单逆序清理，不通配删除两端旧状态：

- 停止本轮 Agent/supervisor/SSH MCP/tmux，并核对完整进程组与残留子进程；未证明结束不删 Runtime guard。
- 删除本轮公钥行和 alias 区块；若配置被其他程序修改，保留其他变更，仅移除本轮区块，不盲目恢复整份备份。
- 删除两端一次性 Agent SSH 密钥和测试目录；原 Codex 专用 HOME、Claude 登录与现有 Controller 保留。
- 本机 ccrun 必须保留。fodelf 新账号仅在清单证明为本轮创建、无活动进程且无后续新增数据时删除；否则报告遗留，等待人工处理。
- 检查两端资源清单均已归零，记录无法清理项和原因。历史 scratch 目录不自动认定为本轮资源。

## 当前恢复点

用户已打开两端终端，并澄清允许 fodelf 临时新建普通专用 Runtime 用户，验收后必须清理账号及本轮 home、公钥、测试目录；之前“不新建用户”的记录是误解，不再适用。本机已有 ccrun 保留。远端现有 fodelf 管理员身份仍不得替代隔离 Runtime。下一步由用户在两端登录终端核对 SSH 配置，再按唯一资源清单准备并验证专用入口。

未创建账号、生成密钥、写授权或变更 ACL/防火墙，没有本轮临时资源需要清理。用户终端认证不代表 Agent 执行进程获得 sudo 权限；需要特权的命令仍由用户登录终端执行，不索取密码或添加临时 NOPASSWD 规则。
