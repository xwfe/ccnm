# Codex 原生执行链：先测会推翻设计的事（P21，2026-09-16）

设计见[双执行入口方案](../plan/runtime-surfaces.md)第 12 节，验收见 ROADMAP 的 P21。本文只写结论、冻结的规则表和它们对设计的改动；每个实验的原始结果、脚本和复跑方式在 toexec 仓库的 [`evidence/v2-c/native-surface/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-c/native-surface/README.md)。

**全程零模型额度**：模型接口是本机假服务，Codex 外面套禁出站沙箱，`HOME`/`CODEX_HOME` 是临时目录。没写 ccnm 代码，没在任何真实 Runtime 上装东西。

## 怎么测的（一段话）

exec-server 跑在本机 Linux 容器里（OrbStack，Debian bookworm，内核 7.0，aarch64，Codex 0.154.0 官方 musl 发行包，以普通用户 `runner` 运行），项目放在 `/srv/p21/work`——**这个路径在 Mac 上不存在**，正好模拟"Agent Node 上没有项目目录"。Codex（macOS，0.154.0）经本机一个逐帧转发的网桥连过去，网桥记下两个方向的全部 JSON-RPC，也能按规则替 exec-server 回答请求（用来试规则表原型）。17 个实验，每个跑 3 遍，逐项比对全部一致（比哪些字段见 toexec 那份 README）。

## 结论

### 1. 工作目录：首版只开交互模式（P21.1）

| 模式 | 做法 | 结果 |
| --- | --- | --- |
| 交互（TUI） | `-C <Runtime 上的根>`，Agent 本机没有这个目录 | **可用**。命令的 `cwd`、`workspaceRoots` 都是 Runtime 路径；`AGENTS.md` 从 Runtime 读 |
| 交互 | 不带 `-C` | 发的是 Agent 本机的当前目录，exec-server 上全部 `No such file or directory`，命令和 patch 都失败 |
| `codex exec` | `-C <Runtime 上的根>`，Agent 本机没有 | 启动即退出，rc 1，`Error: No such file or directory (os error 2)`，没连 exec-server |
| `codex exec` | 不带 `-C` | 同交互不带 `-C`：全部失败，**但 rc 是 0** |
| `codex exec` | Agent 本机建一个同路径的目录 | 可用，`AGENTS.md` 读的是 Runtime 那份（两边放了不同内容的标记文件对照） |

原因在源码：交互模式在默认环境是远端时跳过本机目录检查，`codex exec` 不跳。

**决定：原生链首版只开交互模式，启动时传 `-C <Runtime 根>`。**print 模式要在 Agent Node 上建一个和 Runtime 根**同一绝对路径**的目录，Runtime 根常在 `/home/ccrun/…`、`/Users/ccrun/…` 这类 Agent 账号建不了的地方（macOS 的 `/home` 只读），而且那条路径失败时 rc 仍为 0，出错也看不出来。**代价**：Machine API（`ccnm rpc`）只有 print 模式，Orchestrator 经 Machine API 起的 Codex 会话继续走 MCP 七工具。

### 2. Codex 实际会调哪些方法（P21.2）

默认模型本身就走 Code Mode：模型只看到一个 JavaScript `exec` 工具，里面嵌着 `exec_command`、`write_stdin`、`apply_patch`、`view_image`（ccnm 现在关着它，本轮单独打开测了）、`skills__list`、`skills__read`、`clock__curr_time`。

| 时机 / 工具 | 发出的方法 | sandbox |
| --- | --- | --- |
| 启动 | `initialize`（`resumeSessionId: null`）→ `initialized` → `fs/getMetadata` 从根往上逐级查 `.git` 直到 `/`（重复 3–4 遍）、根下的 `.agents/skills`、`AGENTS.override.md`、`AGENTS.md` → `environmentConfig/read`（`cwd` 为根）→ 有 `AGENTS.md` 时 `fs/readFile` | 全是 `null` |
| `exec_command` | `process/start`、`process/terminate`；输出走服务端通知 `process/output`、`process/exited`、`process/closed` | 在 |
| `apply_patch` 新增 / 修改 / 移动 / 删除 | `fs/getMetadata`、`fs/readFile`、`fs/writeFile`、`fs/remove` | 在 |
| `view_image` | `fs/getMetadata`、`fs/readFile` | 在 |
| `skills__list`（执行端） | 除启动时那次 `.agents/skills` 查询外没有新请求（目录不存在时） | — |

**本轮一次都没出现的方法**：`fs/open`/`readBlock`/`close`/`readDirectory`/`walk`/`canonicalize`/`copy`/`createDirectory`、`process/read`/`write`/`signal`、`http/request`、`capabilityRoots/discoverV1`、`environment/info`/`status`。

**Agent 本机不碰项目**：两边同一路径放不同内容的 `AGENTS.md`，发给模型的是 Runtime 那份。远端项目里放一份 `.codex/config.toml`（改模型，外加一个会在 Mac 上 `touch` 标记文件的 MCP server），在 Codex 里确认信任目录之后，**这份配置没有被读取**：没有对应的 fs 请求，模型没变，Mac 上没有标记文件。

**上层目录是 Git 仓库时**，Codex 找到上层 `.git` 之后，会接着去那个仓库根（在 workspace 根以上）查 `AGENTS.md`、`AGENTS.override.md`、`.agents/skills`。

### 3. 提权有两条路，只看"sandbox 在不在"挡不住（P21.2）

交互模式的审批策略是 `on-request`，模型可以申请提权，人在 TUI 里点同意：

- **命令提权**（`sandbox_permissions: "require_escalated"`）批准后，`process/start` 的 sandbox 是 `null`。
- **越界 patch**（往 `/home/runner/outside/` 加文件）在发出前就先问人，批准后 sandbox **仍然在**，只是条目里多了一条 `{"path": {"type": "path", "path": "file:///home/runner/outside"}, "access": "write"}`。

没有规则时，**这两个文件都写到了工作区外**。所以规则表必须逐条核对 sandbox 的内容。

还有第三条：带着放宽条目的 patch 被拒之后，Codex **不再询问**，自己改发 `sandbox: null` 重试一次。

### 4. 拒绝时回什么（P21.3）

- **exec-server 自己的"不存在"**：`-32004`，`No such file or directory (os error 2)`（sandbox 为 `null`，或带 sandbox 但父目录存在时）；带 sandbox 且父目录也不存在时是 `-32603`。
- **替 exec-server 回答根以上的 `.git` 查询**：照 `-32004` 原样回。在上层目录是 Git 仓库的环境里这样回答后，Codex 发出的请求集合与"上层确实没有仓库"时一致，只差上溯重复的遍数；上面那几条仓库根以上的 `AGENTS.md` 查询也就不再发生。
- **拒绝**：回 `-32600` 加一句说明。Codex 客户端把 `-32600` 当 `InvalidInput`（源码 `remote_file_system.rs` 的 `map_remote_error`），模型看到的分别是：命令 `exec-server rejected request (-32600): <说明>`；读图 `unable to locate image at …: <说明>`；patch 只有 `Failed to write file <路径>`，说明被吞掉。patch 被拒后 Codex 自动以 `sandbox: null` 重试一次（不再问人），再被拒就放弃，不循环。

用 Python 写的规则表原型挂在网桥上跑了三组：正常的读、改、移动、删除、跑命令、读工作区内的图**一条没被误拒**；工作区外读图被拒；命令提权、越界 patch 及其 `sandbox: null` 重试全部被拒，**工作区外零写出**。

### 5. Linux 上的沙箱（P21.4）

| 容器条件 | 结果 |
| --- | --- |
| 没装 bubblewrap | 命令和带 sandbox 的文件方法都失败，命令没有执行（`bubblewrap is unavailable: no system bwrap was found on PATH and no bundled codex-resources/bwrap binary`）。失败即拒，但 Codex 随即弹"不带沙箱重试？"——正是第 3 条的路。官方 musl 发行包里没有附带 bwrap |
| 装了 bubblewrap 0.8.0，容器默认 seccomp | 普通用户建不了 user namespace，bwrap 起不来 |
| 装了 bubblewrap，允许 user namespace | 工作区内写入成功；工作区外 `Read-only file system` |

**所以 Linux Runtime 的前提是：装 bubblewrap，并允许执行账号创建 user namespace。**容器里要额外放开 seccomp 才满足，这是容器的限制；真实主机（例如某些发行版用 AppArmor 限制普通用户 user namespace）没测。

### 6. 版本核对不能用 `executorVersion`

Linux musl 发行包 `codex --version` 是 `codex-cli 0.154.0`，握手返回的 `executorVersion` 却是 `"0.0.0"`；macOS Homebrew 构建返回 `0.154.0`（toexec G01）。源码里 `providerId` 由构建提交和目标平台算出（`build_identity.rs`），同一版本在不同平台上不同。**改为**：核对 Runtime 配置的二进制 `--version`，并记录握手的 `providerId`。

### 7. 服务端 `CODEX_HOME` 不能放临时目录

放在 `/tmp` 下时 exec-server 启动就告警 `Refusing to create helper binaries under temporary dir`。对功能的影响没测，按保守处理：ccnm 生成的目录不放系统临时目录。

## P21.5 冻结的方法规则表

P22 照这张表实现。表里没列的方法一律拒。实现时补了几条，下表里标"（P22 补）"，原因见 [P22 记录](p22-exec-serve-2026-09-16.md)。"在根内"指先查原始输入、拒绝 `..`，再解析 symlink 后仍在 workspace 根内（第 12.2 节）；原型只做了字符串前缀比较，symlink 部分由 P22 实现并测试。

| 方法 | 放行条件 | 不满足时回 |
| --- | --- | --- |
| `initialize` | `resumeSessionId` 为 `null` | `-32600`，然后断开 |
| `initialized` | 转发 | — |
| 其他客户端通知 | 不转发 | 丢弃（exec-server 收到未知通知会直接断连） |
| `environmentConfig/read` | 放行；服务端 `CODEX_HOME` 由 ccnm 生成、不含凭据、不在临时目录 | — |
| `environment/info`、`environment/status` | 放行（只返回服务端环境信息） | — |
| `fs/getMetadata`、`fs/readFile`、`fs/readDirectory`、`fs/walk`、`fs/canonicalize`、`fs/open` | 路径在根内，不看 sandbox | 根以上的 `<祖先目录>/.git`：`-32004 No such file or directory (os error 2)`；其余 `-32600` |
| `fs/readBlock`、`fs/close` | 放行；句柄只可能来自放行过的 `fs/open`，而每个会话有自己的 exec-server 进程，句柄跨不了会话 | — |
| `fs/writeFile`、`fs/remove`、`fs/copy`、`fs/createDirectory` | 每个路径参数都在根内，且 sandbox 合规；写入目标按 MCP `apply_patch` 的规则：不写 `.git`、不写穿 symlink、不写根本身（P22 补） | `-32600` |
| `process/start` | `cwd` 在根内，sandbox 合规，`managedNetwork` 为 `null`，`enforceManagedNetwork` 为 `false`；`networkProxy`、`shellSnapshot` 为空；`envPolicy.inherit` 为 `all`、`includeOnly` 为空，且 `exclude`/`set`/`env` 不碰会话标记变量 `CCNM_EXEC_SESSION`（P22 补） | `-32600` |
| `process/read`、`process/write`、`process/signal`、`process/terminate` | 放行 | — |
| `http/request` | 一律拒 | `-32600` |
| `capabilityRoots/discoverV1` 及其余方法 | 拒 | `-32601` |

**sandbox 合规**（fs 写方法和 `process/start` 共用）：

1. 必须存在，`null` 拒。
2. `permissions.type` 是 `managed`，`permissions.network` 是 `restricted`，`file_system.type` 是 `restricted`。
3. `workspaceRoots` 恰好是 `[workspace 根]`，`cwd` 在根内。
4. `file_system.entries` 的每一条都在下面 7 条之内——这就是实测 workspace-write 的完整形状：根目录读；`project_roots` 写；`slash_tmp` 写；`tmpdir` 写；`project_roots` 下的 `.git`、`.agents`、`.codex` 读。**任何 `{"type": "path"}` 条目都拒**，Codex 批准越界写时加的就是它。
5. `useLegacyLandlock` 为 `false`。Windows 相关字段不校验。
6. （P22 补）`temporaryDirectories` 为空——`tmpdir` 条目按它解析，客户端填了就能指向任何地方；出现上面没列的 sandbox 字段就拒；条目的 `missing_path_behavior` 只接受 `skip`。

**命令能读什么不归这张表管**：`process/start` 放行后，命令能读 ccrun 能读的一切，写入受上面这份 sandbox 限制，与 MCP 的 `exec_command` 一样（第 12.2 节）。

## 对设计和后续验收的改动

- [双执行入口方案](../plan/runtime-surfaces.md)第 12 节：首版只开交互模式；`.git` 的回答形状定为 `-32004`；补上 Linux Runtime 的前提。
- ROADMAP P22.3：版本核对从"`--version` 和 `executorVersion`"改成"`--version`，并记录 `providerId`"（依据第 6 条）；`CODEX_HOME` 不放临时目录（第 7 条）。P22.4 指向本文的规则表。
- ROADMAP P23.3：原生链只接受交互模式，传 `-C <Runtime 根>`；print 模式在创建会话前拒绝。
- ROADMAP P24.1：Linux Runtime 先确认 bubblewrap 和 user namespace。

## 没测到的

- 只用了 Codex 默认模型（本身就是 Code Mode）。ccnm 为未实测模型关掉 Code Mode 的那条路径下，工具面没测。
- `fs/open`/`readBlock`、`readDirectory`、`walk`、`copy`、`createDirectory` 本轮没被调用，规则表对它们的条件是按方法语义定的，不是按实测请求定的。
- 路径边界只测了"根内 / 根外"，symlink、`..`、根内指向根外的链接留给 P22。
- 规则表原型是 Python 写在网桥上的，只用来观察 Codex 对拒绝的反应；它不是实现，也没测断线、并发和写锁。
- Linux 只在容器里测，真实主机上 user namespace 的限制没测；macOS 上作为 Runtime 的 Seatbelt 行为引用 toexec G06，本轮没重测。
- `environmentConfig/read` 返回的主机名是否要抹掉，没测 Codex 能不能接受抹掉后的响应，本表先放行。
