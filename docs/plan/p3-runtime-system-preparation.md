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

### 独立组准备被残留系统进程阻止

用户执行组修正得到 `Runtime processes still active`。通过 fodelf 管理员身份只读 `ps`（未重新登录临时 Runtime）复核，UID550 仅有 PID9447、PPID1、PGID9447 的 `/usr/sbin/distnoted`；没有测试 shell/Agent。账号主组仍为20，脚本在写清单/创建组之前退出。不得放宽进程检查，也不能把系统进程忽略后宣称完整清理。

下一步由用户执行 `sudo launchctl bootout user/550`，仅注销本轮临时账号的用户服务域；不触及 fodelf UID501 或本机服务。随后重跑原组修正脚本，它仍要求 UID550 无进程才修改。任一命令报错则保留输出，不自动 kill、不循环重试。当前无权限读取该 launchd 域（返回 Operation not permitted），域是否成功注销及残留是否归零须以管理员执行结果复核；不得预先宣称修复成功。

### 独立组复验与构建传输阻塞

用户注销临时用户服务域后成功执行组修正。本轮使用全新 SSH（禁用复用/agent 转发、明确临时 key）验证 UID/GID550、无 staff/admin、真实 HOME `/Users/ccnmp3test`。fodelf home、已知两 Provider 目录、SSH 目录与 login Keychain 均不可读，两个已知 Docker socket 不可写，sudo 非交互拒绝。证据见 `tests/fixtures/p3-runtime-identity/isolated.json`；这不是完整提权或 egress 审计。

`cargo build -p ccnm-cli` 通过，尝试向 Runtime 自建 0700 目录 `/Users/Shared/ccnm-p3-runtime.Iy21vY` 上传独立当前构建（原文件 41,824,312 bytes，gzip 8,554,304 bytes）。未覆盖现有安装。原始流上传中止、压缩上传 180 秒超时，续传又发生 SSH server not responding；128 KiB 小块耗时22.12秒且文件增量163,840 bytes，与本次发送量不符。不能将拼接结果视作完整构建，不再盲目重试。没有执行该二进制，没有启动 MCP/模型，也没有创建测试 config/project/state。

后续只读发现 UID550 的残留 cat PID16103；以 lsof 的 stdout 路径精确核对为本轮 `ccnm.gz.part` 后发送 TERM，确认 PID 消失，再删除两个不完整文件，并以 rmdir 成功移除空部署目录。没有终止其他账号进程或改网络配置。客户端 SSH 退出不代表远端上传进程立即结束，本次是实际反例；网络根因尚未定位，不能以小连接成功宣称大流传输已修复。

当前：独立组已建立；账号/组、SSH 公钥、本机测试密钥、两端清单与上传准备脚本继续保留用于接续，尚未完成最终清理。部署目录已清理。P3.5 保持 blocked，下一步先解决有校验和及远端结束确认的构建传输，再进行当前 build MCP 与公共 Agent 链路验收；不得借此更换网络产品或放宽安全门禁。本轮计划检查和 Python 全量25通过，未修改 Rust，不重报历史 Rust 全量为本轮测试。

### 最新恢复点：当前构建 MCP 七工具通过

网络恢复后已完成有完整哈希校验的分块部署和真实 v3 MCP 七工具/编译/Git 验证，详见 `docs/research/p3-isolated-runtime-2026-09-08.md`。当前 Runtime 测试目录为 `/Users/Shared/ccnm-p3-runtime.R7Enw5`；与已清理旧目录 Iy21vY 区分。传输块和压缩包已删除，构建、wrapper、配置、project/state 保留；本机资源清单已同步。

独立组和身份隔离保持，尚未启动模型。下一步接通公共控制链，不能假定临时用户拥有原用户的 xdwmbp alias/SSH身份；不复制已有私钥，不转发agent或绕过host key验证。全部账号/组/公钥/临时目录仍需要最终按清单清理。本轮辅助脚本 `/tmp/ccnm-p3-upload-parts.py`、`/tmp/ccnm-p3-runtime-smoke.py` 和探针日志属于本轮本机临时产物，清理时一并处理，不作为接续必须依赖。

### 最新恢复点：Codex 公共链路通过，准备 Claude 反向入口

详见 `docs/research/p3-public-codex-2026-09-08.md`。Codex公开print、interactive、detach/reattach、Controller重启、Ctrl-D及transport故障后stop已有实测；本轮Controller和已记录session进程组已清理，官方临时trust entry恢复。现有登录与官方会话历史保留。

新增保留资源：本机 `/tmp/ccnm-p3-agent-m8wmi9ng`（独立config/state/tmux、SSH配置私有备份、收到的local-runtime.pub），本机SSH唯一alias block `ccnm-p3-r7enw5`；远端 setup 下的 runtime-control.toml、control-state、本轮新密钥 local-runtime-key 及.pub。私钥不出生成节点。

本机待执行 `sudo /bin/bash /Users/bing/xdw/ccnm/scripts/p3-authorize-local-runtime.sh --apply`。仅给UID504已有ccrun追加本轮公钥，任何已有路径的symlink/owner/权限异常即拒绝；开始前生成 `/var/db/ccnm-p3-local-20260908`，备份已有公开authorized_keys并记录新增行/目录。该root清单目前尚未创建。清理时只删除记录的追加字节，保留并发修改；仅当本轮创建且为空时删除目录，始终保留原ccrun账号。不要盲目回滚整份authorized_keys或SSH config。

### 本机公钥已追加，但 Remote Login 准入仍阻止 ccrun

用户报告本机授权脚本成功，root清单 `/var/db/ccnm-p3-local-20260908` 已由该脚本创建；我方尚未读取该root清单。实际从fodelf使用新key连接，服务端接受公钥后关闭连接，没有执行id。虽然外层SSH返回0，不能记为登录成功。

只读证据：本机ccrun仍UID504/主组staff，无admin；`dsmemberutil` 确认不属于 `com.apple.access_ssh`。该服务组只嵌套admin组（GeneratedUID匹配），无直接用户成员；`/etc/pam.d/sshd` account阶段强制 `pam_sacl.so sacl_service=ssh`。这说明标准sshd公钥配置之外还有系统账号准入限制。证据见 `tests/fixtures/p3-local-runtime-access/result.json`，没有读取凭据或修改组。

最小变更：只将已有ccrun加入 `com.apple.access_ssh`，允许其通过Remote Login账号准入；不加入admin、不改主组/UID/密码/防火墙，也不开放所有用户。该准入对ccrun既有的有效认证方式同样生效，并非仅限定本轮key。原组、其他成员和ccrun账号保留。

### 用户已确认临时准入，脚本待管理员执行

用户明确批准「临时将 ccrun 加入 com.apple.access_ssh，验收后移除」。本轮只读复核与上节一致：ccrun UID504、主组staff、无admin；`com.apple.access_ssh` GID399，只嵌套admin，`GroupMembership` 属性不存在（无直接成员）；`dsmemberutil` 判定 ccrun 非成员。

用 `scripts/p3-grant-local-ssh-access.sh`，两个动作互为逆操作，都必须由用户在本机 bing 终端 sudo 执行：

- `--apply`：核对 UID504、root清单目录属性、组 GID399，确认 ccrun 当前非成员后，先写清单 `/var/db/ccnm-p3-local-20260908/ssh-access-group.txt`（记录变更前的非成员状态、`NestedGroups` 与 `GroupMembership` 原值），再用 `dseditgroup` 追加直接成员。清单已存在即拒绝，不覆盖上一轮现场。
- `--revert`：要求清单存在且记录变更前为非成员，移除直接成员关系后复核已非成员，且组的嵌套与直接成员回到清单记录值才删除清单；发现别处并发修改则保留清单并以非0退出，等人工核对，不回滚他人变更。

判定成员前后都执行 `dsmemberutil flushcache`，避免本地缓存把旧结果当结论。脚本不读取任何认证文件，不修改 sshd 配置或 PAM 策略。

准入生效后复验：从 fodelf 用本轮 key 连接本机 ccrun，必须实际拿到 `id` 输出（UID504）才算登录成功，外层 SSH 退出0不算；随后按第4步重做凭据/Docker/sudo 只读隔离检查，再接 Claude 公共链路。最终清理时，`--revert` 与删除公钥行、root清单同属本轮资源，逆序执行。

本轮新增 3 个脚本入口测试，Python 全量28通过；未修改 Rust，不重报历史 Rust 数字。执行前系统未发生任何变更。

### 准入已生效、反向登录通过，但凭据隔离验收失败

用户执行 `--apply`。只读复核：ccrun 已是 `com.apple.access_ssh` 成员，该组 `GroupMembership` 只有 ccrun 一个直接成员，`NestedGroups` 仍只有 admin，ccrun 主组仍 staff、无 admin，UID504 未变。

反向链路首次真正成功：从 fodelf 用本轮 key（私钥仍只在 fodelf）连接 `ccrun@xdwmbp`，内层 SSH 退出0并实际返回 `uid=504(ccrun)`、真实 `HOME=/Users/ccrun`，不是上一轮那种只有外层退出0的假成功。禁用连接复用与 agent 转发，保留 host key 严格校验。内层 ssh 需要 `-n`，否则会吃掉外层 `bash -s` 的脚本，本轮实际踩到并修正。

通过项：Claude `.credentials.json` 与 Codex `config.toml` 不可读，8 个 SSH 私钥候选全部不可读，`sudo -n` 拒绝，无 CODEX_HOME/CLAUDE_CONFIG_DIR/SSH_AUTH_SOCK。

隔离不接受，缺口有两层，证据见 `tests/fixtures/p3-local-runtime-access/admitted.json`：

- staff 穿透：`/Users/bing` 为 0750、属组 staff，而既有 ccrun 主组就是 staff，Agent home 及 `.claude`/`.codex`/`.ssh`（均0755）全部可列。
- 叶子文件本身 world-readable：`/Users/bing/.codex/auth.json` 与 `.bak` 为 0644，Runtime 可读；`.claude.json`（126KB）可读；两个 Agent 目录下共 68 个叶子文件可读，含各 settings 配置与 Codex 会话历史 sqlite。

只做 `[ -r ]` 判定和 `ls -l` 元数据，没有读取任何凭据内容，没有复制文件。这个缺口是既有环境的文件权限状态，不是 ccnm 引入的，但它使 Runtime 身份能拿到 Agent 凭据，P3.5 不能通过。

同时修正一处自身判定缺陷：上一版探针用 `[ -e ]`，权限拒绝时返回假，把 login Keychain 和 Docker socket 误报成 `absent`。重测确认真实原因是父目录 `/Users/bing/Library`、`/Users/bing/.orbstack` 均为 0700 拒绝访问，不是 TCC 也不是不存在；不可读/不可写的结论成立，但原因不能记错。

在隔离修正方案确定前不启动 Claude 公共链路。修正涉及改既有 ccrun 主组或改个人 home 权限，超出「仅临时准入」的授权范围，需用户单独决定；在此期间建议先执行 `--revert` 关闭远程准入，因为验证结论已取得，继续开启只是让可读的 Codex 凭据多一条远程路径。

### 用户选定：给既有 ccrun 建独立主组

用户批准方案「给 ccrun 建独立主组」，不收紧 `/Users/bing` 权限、不加 ACL。理由是关掉 staff 这道门比逐个改用户文件权限更根治，且只改临时验收用的 Runtime 账号，不动个人环境。

只读侦察确认可行：`/Users/ccrun` 为 504:20、0700；组名 `ccrun` 与 GID 504 均未占用；UID504 仅有 PPID1 的系统 `distnoted`，无用户会话或服务。

用 `scripts/p3-isolate-local-runtime-group.sh`，apply/revert 互逆，都要用户在本机 bing 终端 sudo 执行：

- `--apply`：核对 UID504、home 属性与 NFSHomeDirectory、当前主组确为 staff、组名/GID 504 未占用、root 清单目录属性，并要求 UID504 无活动进程；先写清单 `local-group-resources.txt`（记录 `previous_primary_gid=20` 和被改属组的路径），再建组、改主组，最后校验 `id ccrun` 不含 20/80。
- `--revert`：要求清单记录变更前为 staff 且当前主组确为本轮值，改回 GID20、属组还原、删除本轮组和清单。

只对 `/Users/ccrun`、`.ssh`、`authorized_keys` 三条已知路径改属组，不递归、不碰 ccrun 其他既有文件——这三条 mode 已是 0700/0600，改属组是防止日后放宽 mode 时缺口重开。改主组不影响已在运行进程的既有组身份，所以脚本拒绝在有 UID504 进程时执行，并提示 `sudo launchctl bootout user/504`；不自动 kill、不循环重试，与上一轮 fodelf 侧处理一致。

注意这个方案关的是穿透那道门，`.codex/auth.json` 仍是 0644，对本机其他 staff 成员依然可读。这是既有环境的独立问题，本轮不改用户文件权限，如实留作待办。

新增 2 个入口测试，Python 全量30通过。执行前系统未再变更。

### 独立组已生效，本机凭据隔离通过

首次执行按设计被 UID504 的活动进程拒绝。只读核对该进程是 PPID1、PGID1492 的系统 `distnoted`，启动时间正是本轮首次 SSH 连接的时刻，即 launchd 拉起的用户服务域残留，无用户 shell 或 Agent 进程。用户执行 `sudo launchctl bootout user/504` 后重跑成功。

变更后只读复核：`id ccrun` 为 uid504/gid504(ccrun)，staff 已消失、无 admin，`com.apple.access_ssh` 准入保留；`/Users/ccrun` 为 504:504、0700。

全新 SSH 连接复验隔离，结果见 `tests/fixtures/p3-local-runtime-access/isolated.json`：Agent home 不可列，`.claude`/`.codex`/`.ssh`/Keychains 全部权限拒绝，此前可读的 `.codex/auth.json`、`auth.json.bak`、`.claude.json`、`settings.json`、`session_index.jsonl` 均转为拒绝，两个 Agent 目录可读叶子数从 68 降到 0，两个 Docker socket 不可写，`sudo -n` 拒绝，真实 HOME 为 `/Users/ccrun` 且无 CODEX_HOME/CLAUDE_CONFIG_DIR/SSH_AUTH_SOCK。本机 Runtime 侧凭据隔离接受。

探针同时改掉了之前的判定缺陷：改用 `ls -ld` 的实际错误文本区分「权限拒绝」「不存在」，不再用 `[ -e ]`。

遗留待办（不在本轮变更范围）：`/Users/bing/.codex/auth.json` 与 `.bak` 仍是 0644，本机其他 staff 成员依旧可读。这是既有环境问题，与 ccnm 无关，按用户选择本轮不改个人文件权限。
