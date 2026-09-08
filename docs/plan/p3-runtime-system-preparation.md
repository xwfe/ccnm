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

## fodelf 临时账号准备

使用 `scripts/p3-create-runtime-user.sh`，仅针对 fodelf 登录用户执行；固定临时账号 `ccnmp3test`、UID 550、home `/Users/ccnmp3test`。执行时重新核对名称、UID 和目录未占用，碰撞即拒绝，不覆盖现有账号。密码字段为不可用值，不设置可登录密码、不加入 admin、不修改 SSH 服务或 sudoers。公钥入口另行准备和实测。

`--check` 无写操作；`--create` 必须由用户在 fodelf 终端通过 sudo 执行。特权动作开始前创建 root 所有的 `/var/db/ccnm-p3-account-20260908/resources.txt` 清单；部分失败保留记录，禁止反复运行或自动清除。只有所有创建步骤成功才写入 `state=created`。

清理账号前核对清单、目录服务中的 UID/home/RealName、无该 UID 活动进程，并按本轮清单移除授权和测试文件。home 必须只剩空目录才能用 `rmdir` 删除；发现未知文件或任何属性不匹配则停止。再删除对应目录服务账号和 root 清单；不使用递归删除处理未知残留。部分创建失败需按实际成功步骤恢复，不能假定 `state` 存在。

## 当前恢复点

用户报告两端 `sshd -T`：公钥认证开启、authorizedkeyscommand 为 none、authorizedkeysfile 为 `.ssh/authorized_keys`、authenticationmethods 为 any。这只证明用户报告的默认配置，不证明 Match/PAM/系统访问组或真实新账号 SSH 登录成功。

用户已执行创建脚本；本轮 SSH 只读复核 `ccnmp3test` 为 UID 550、home `/Users/ccnmp3test` 为 0700，RealName 匹配本轮标记，组列表无 admin。root 清单内容尚未由我方读取，不将账号创建等同于凭据隔离或 SSH 验收。

本轮资源清单（均需结束后清理）：

- fodelf：账号 `ccnmp3test`、home `/Users/ccnmp3test`、root 清单 `/var/db/ccnm-p3-account-20260908`；上传目录 `/tmp/ccnm-p3-setup.TnaRle`，含创建脚本、公钥 `runtime.pub` 与授权脚本。
- 本机：`/Users/bing/.config/ccnm/p3-ssh-0a8i4v2q`，含一次性 SSH 密钥对及 `resources.json`。私钥仅在本机，未传输，未读取内容。
- 授权脚本已创建（用户报告成功且 SSH 实测通过）：远端 `/Users/ccnmp3test/.ssh` 和 `authorized_keys`，以 root 清单 `ssh-resources.txt` 记录。现有 `.ssh` 一律拒绝覆盖。

`p3-authorize-runtime-key.sh` 只允许安装指纹 `SHA256:AlJxpK96ks8KDg0woK1tSluntPXRt9XV0ZA/Rp2HPZ4` 的单行 ed25519 公钥，禁止 agent/端口/X11 转发及 user rc；不改 SSH 服务策略。脚本已上传且 SHA-256 与仓库一致：`cf0f8ed8552512174ae27059acc2ccf8c79d3f600ccb00e53b30447d49547988`。此步骤用户已执行成功，后续实测与当前阻塞见下一节。

上一轮 5 个脚本入口测试、Python 全量 24 通过；本轮增加独立组脚本入口检查，Python 全量 25 通过。清理尚未验证；未改 ACL/防火墙、未启动模型。本机已有 ccrun 和两端 Agent 登录保持不变。

## SSH 实测后的主组修正

公钥安装后，实际 SSH 登录已通过，UID 550，真实 HOME 为 `/Users/ccnmp3test`，无 CODEX_HOME/CLAUDE_CONFIG_DIR/SSH_AUTH_SOCK。已知 Claude 凭据、login Keychain 不可读，4 个非公开 SSH 候选条目均不可读，两个已知 Docker socket 不可写，`sudo -n true` 拒绝；没有读取任何凭据内容。结果见 `tests/fixtures/p3-runtime-identity/result.json`。这些不是完整提权审计或 egress 验收。

隔离未接受：fodelf home 是 501:20、0750，临时账号继承 staff 主组，能读取 Agent 的 `.claude` 和 `.ssh` 目录。不能以凭据叶子文件不可读掩盖这个已知缺口。修正只改变临时账号，不向个人 HOME 添加 ACL。

待执行 `scripts/p3-isolate-runtime-group.sh --apply`：核对账号、目录所有权、无活跃 UID550 进程、组名/GID550 未占用后，创建本轮 `ccnmp3test` 独立组并改临时账号及 home/.ssh/authorized_keys 主组；不递归修改其他路径。变更前登记 root 清单 `group-resources.txt`；碰撞/部分失败保留现场，不重试覆盖。用户执行路径是 `/tmp/ccnm-p3-setup.TnaRle/isolate-runtime-group.sh`，上传 SHA-256 与仓库一致：`bf5dacccee51538ba232052535c6d4169bb5ac5896ad25606be8111f16b46899`。

清理新增组时，先完成前述账号/文件清理，确认没有其他账号使用 GID550、组标记及本轮清单匹配，再删除本轮组及清单；发现其他使用者或未知残留则停止，不自动恢复成 staff 继续验收。当前组修正尚未执行，未启动真实 Agent。
