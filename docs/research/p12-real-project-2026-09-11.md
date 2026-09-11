# P12 真机记录：Linux Runtime 上的远端真实项目 dogfood（2026-09-11）

这一轮做的是 P12：把 Remote Workspace MCP 放到**一台真的 Linux 机器、一个真的专用执行身份、一棵真的项目树**上用一遍，并让一个**真实的 Claude Code** 在那棵树上完成一次真改动。顺序、判据和资源清单在[会话计划](../plan/p12-real-project-session.md)里，执行前已逐项获得用户授权。

机器可判的部分写在 [p12-dogfood-20260911.json](p12-dogfood-20260911.json)，退出码 0。人要看的部分写在下面。

## 一、环境

| | 机器 | 角色 |
| --- | --- | --- |
| Host（外部 MCP 客户端） | `fodelf`（macOS 26.3） | 官方 Claude Code 2.1.268 + `ccnm mcp bridge` |
| 客户端（脚本那一腿） | 本机 `xdwmbp`（macOS） | provider-neutral MCP 客户端 + `ccnm mcp bridge` |
| Runtime（权威工作树） | `hpsrv`（**Debian 13 trixie，x86_64**，6 核 15 GiB） | `ccnm internal mcp-serve`，执行身份 `ccrun`（uid 1002、主组 ccrun、没别的组） |

选 Linux 是这一轮的重点：P3 到 P11 的真机证据全是 macOS → macOS，而产品一直声称"项目在 Linux 开发机"是主场景。

两端 ccnm 是同一个提交（`069fa68`）的构建：macOS `234e2b4e…`（Host 与客户端），Linux `9a52d35c…`（Runtime，在那台机器上自己编译的，`cargo build --release` 1 分 10 秒）。被替换的都留了可回退副本（fodelf `~/.local/bin/ccnm.pre-p12` = P11 的 `93280d81…`）。

**没开 `allow_unconfined_exec`。** `ccrun` 是专用受限账号，隔离审计本来就该放行；开了那个开关等于把要验的东西绕过去。

项目是 ccnm 自己的源码树：本机 `git bundle` → scp → `git clone`，所以 Runtime 上是一棵**真的 git 仓库**（`git status` 能答话），而且 `ccrun` 不需要任何出站凭据就拿到了它。

## 二、四条验收的结果

| 验收 | 结果 |
| --- | --- |
| **P12.1** 真项目走完七工具、断开重连、资源归零 | **通过**，退出码 0 |
| **P12.2** 执行身份受限 + 项目 toolchain 实际可用 | **通过**（egress 明确不作保证，见第六节） |
| **P12.3** 六条失败路径各有可追溯结果、无孤儿 | **通过** |
| **P12.4** 文档与 v1.x 冻结 | 本轮完成（见[支持矩阵](../support-matrix.md)与[契约](../protocol/remote-workspace-mcp-v1.md)） |

### 机器判的部分（脚本，没有模型参与）

`scripts/p12_dogfood_check.py` 从客户端拨真 ssh 过去，判据全在 Runtime 侧：

- **身份**：`id -un` 是 `ccrun`，组只有 `ccrun`（不在 sudo/admin/wheel/adm/docker/staff）；`sudo -n true` 退出 1；`/var/run/docker.sock` 不可写；`~/.ssh` 里只有 `authorized_keys`，没有任何私钥候选；`SSH_AUTH_SOCK` 在 Runtime 进程里不存在；`/home/bing` 读不到（0700）。
- **工具链**：`cargo 1.98.1`、`rustc 1.98.1`、`node v24.21.0`、`npm 11.19.0`、`git 2.47.3` 全部由 `exec_command` 在那台机器上问出来——**不是**我们 ssh 进去问的。
- **改-编译-读-收回**：`search_text` 命中 `crates/ccnm-core/src/lib.rs`，`apply_patch` 往里插一行编译不过的代码，`cargo build -p ccnm-core` 退出 101 且错误指着那个文件（这一条同时证明 patch 落到了工作树、Runtime 上真有工具链、编译的就是我们改过的那棵树），`read_output` 分两页取出 557 + 64 字节的失败输出，改动收回去之后 **Runtime 上的 `git status --porcelain` 为空**，`cargo test -p ccnm-core --lib runtime::` 18 passed，`cargo test --workspace` 退出 0。
- **失败路径**：writer busy、只读 workspace 上请求 coding 不降级、没 opt-in 的 workspace 打不开、协议号 99 被 `CCNM_E_VERSION` 顶回去且 stdout 上一个字没说、远端进程被 kill、Host 被 kill。
- **泄漏扫描**：Host 看到的全部文本（含真实项目的构建输出）没有私有标记。

### 真实 Claude Code：一次真改动

给它的任务是一件真事：ccnm 在 Runtime 上缺 ripgrep 时提示 `brew install ripgrep`，而这台 Runtime 是 Debian，那句提示把人指错方向；让它找到、改成同时照顾两种系统、在远端跑那一组测试、报告结果。除了这一个 MCP server 之外不给它任何工具。

它做的和它说的：

- 改了 `crates/ccnm-core/src/mcp/search.rs:150`，只动括号里那一段（`brew install ripgrep` → ``brew install ripgrep` on macOS, `apt install ripgrep` on Debian/Ubuntu``）；
- 在**远端**跑 `cargo test -p ccnm-core --lib mcp::search`，19 passed；
- 顺手按仓库 `CLAUDE.md` 的习惯做了一个粒度 commit（`726e279`），并且**没有**去改那台机器的 git 全局配置，而是用 `-c user.name/user.email` 只给这一次提交带上；没有 push。

Runtime 侧复核（不采信自述）：磁盘上那一行就是它说的那一行，文件属主 `ccrun:ccrun`，`726e279` 确实在历史里、一个文件一行改动，`git status` 干净。那个提交随后经 `git bundle` 带回本仓库，也就是这份记录所在的仓库——**这一轮的产出之一是这个仓库自己的一个提交，而它是在一台远端 Linux 机器上、由一个只有七个工具的 Agent 改出来的。**

## 三、Linux 第一次跑门禁：两个红

在 Runtime 上第一次跑 `cargo test --workspace` 就红了两个，**都是真问题**，都已修（`069fa68`）：

1. **`SystemRunner::run` 的超时只杀 leader。** `process::tests::timeout_kills_the_child` 五次五红：100ms 的超时等了 10.0 秒。这条路径是每条非交互 ccnm 命令都要走的（ssh transport、doctor 探针都在里面），它在超时时只调 `child.kill()`，命令自己起的子进程接着占着管道，于是排水线程读到命令**自然结束**才回来。P11 那轮修的是另外两条带 watchdog 的路径（`run_captured`、`stream_lines`），这条被漏下了，而共用的注释写着"两个 spawn 点"——第三个就藏在那句话里。macOS 一直是绿的，因为 `sh -c 'sleep 10'` 这个形状 bash 会 exec，leader 本身就是 sleep；Debian 的 `sh` 是 dash，它连这个形状都 fork。**平台差异不是缺陷，leader-only 才是**，而它在 macOS 上也能碰到（一个后台子进程就够），所以回归测试现在用 `sleep 30 & wait` 把**三**个入口都跑一遍。
2. **测试里的 `date -r` 不可移植。** patch 的扫尾测试用 `touch -t` 设 mtime，时间戳靠 `date -r <秒>` 格式化：BSD date 把 `-r` 读成"这是个时间戳"，GNU date 把它读成"这是个文件名"。于是 Linux 上 stamp 是空的，测试死在 `touch failed`。改成 `std::fs::FileTimes`，不再有子进程可以分叉。

修完两端都是 **fmt、clippy 通过，676 passed / 0 failed**；macOS 另跑 `--test-threads=64` 全绿。

## 四、真机才暴露的另外三件小事

- **`ccnm doctor <只给外部 MCP 用的 workspace>` 回 `CCNM_E_INTERNAL`。** 一个没有 `agent` 的 workspace 是配置校验明确允许的形状（就是给外部 MCP client 用的），但 doctor 把它当成内部矛盾：`workspace 'p12rust' passed validation but its Agent Node is missing`。它不挡任何事（这一轮所有判据都不经过 doctor），但报成 INTERNAL 会让人去查一个不存在的配置错误。**已在随后一轮修掉**：`Resolved.agent` 变成 `Option`，受管入口经 `require_agent()` 按名字拒绝（并指向 `ccnm mcp bridge`），doctor 对这种 workspace 报 policy 与它能证的那几行、把 Agent 那一半逐行 SKIP。安全那一行仍然是 SKIP 而不是绿——它属于工具真正跑起来的那个账号，没有 Agent 探针就问不到。
- **`ccnm_bin` 的默认值写不进配置文件。** 默认是 `~/.local/bin/ccnm`（注释说明了"`~` 由远端登录 shell 展开"），但配置校验要求这个字段是绝对路径，所以把默认值照抄进 `config.toml` 会被 `CCNM_E_CONFIG` 拒。不影响使用（省掉这一行就是默认），但"文档里的值不能填进配置"是会绊人的。**已在随后一轮修掉**：该字段接受 `~/` 开头，仍然拒绝别人的家目录、`..` 和需要引号的字符。
- **一次没能复现的 transport 掉线。** 六次完整跑里有一次在 cycle 中途断了：ssh 退出 255、stderr 一个字没有、内核日志没有 OOM、写锁按设计停在 `held`、远端没有孤儿进程。之后专门用同一条长命令（`cargo test --workspace`，12–17 秒）连跑三次没有复现，最后三次完整跑也都干净。**没有解释就是没有解释**：记在这里，不算进任何判据。

## 五、工具链：装什么、装在哪、谁维护

这是 P12.2 里最容易被文档糊过去的一句话，本轮把它变成了可重跑的两条命令。

- **系统级只装两类东西**（`scripts/p12-provision-linux-runtime.sh`，要 root）：Rust 链接期要的 `gcc libc6-dev make`，以及 **ccnm 自己**要的 `ripgrep`——`search_text` 不自己扫文件，它调 `rg`，Runtime 上没有 rg 就等于七工具少一个。这台机器上因此新增 36 个包（含依赖），清单记在 `/var/lib/ccnm-p12-20260911/packages-installed`，`--revert` 按这份清单精确 purge，不靠 `apt autoremove` 顺手删别人的孤立包。
- **工具链本体装在执行身份自己的 home 里**（`scripts/p12-runtime-toolchain.sh`，**不要 root**）：`~/.rustup`、`~/.cargo`（rustup stable + clippy + rustfmt）、`~/.local/node-v24.21.0-linux-x64`。不碰 `/usr`，不碰别的账号——"不污染其他用户"是这么做到的，不是靠约定。
- **PATH 是真正的坑。** `exec_command` 的命令跑在一条**非交互** ssh 会话里。Debian 的 `~/.bashrc` 第 6 行就 `return`，而 rustup 默认把 PATH 写在文件**末尾**和只有 login shell 才读的 `~/.profile`——两个位置在这条会话里都不生效。照默认装完，ccnm 报的是 `cargo is not installed on the Runtime Node, or is not on its PATH`，看着像没装。脚本因此用 `--no-modify-path` 自己写，并把那一块插在那个 `return` **之前**；判据是从客户端问一次 `ssh ccrun@runtime 'command -v cargo node npm'`，实测通过。
- **谁维护：Runtime Node 的管理员。** ccnm 不装、不升级、不代管版本，也不知道项目需要什么。这一条现在写进了[运维手册](../operations.md)。
- **装工具链需要出站网络。** 顺带暴露了这台机器的出口不是均质的：`static.rust-lang.org` 直连没问题，`nodejs.org` 会 TLS reset（`curl: (35) Recv failure`）。脚本因此支持换源 + `--node-sha256` 固定哈希，而那个哈希取自**官方** `SHASUMS256.txt`（在能连上官方的那台机器上取），不信镜像自己给的清单。

## 六、egress：不作保证

`ccrun` 可以自由出站——工具链就是这么装上去的。ccnm 没有、也不打算有任何网络策略：`exec_command` 能跑任意程序，任意程序能联网。**所以"隔离"这个词在本项目里只覆盖身份、文件和写互斥，不覆盖网络。** 要限制出站得在 tailnet ACL、防火墙或 OS 网络策略上做，那不属于 ccnm 的边界，`ccnm doctor` 也查不到。

## 七、资源与清理

按用户选择：**账号和工具链保留**（`hpsrv` 以后继续当 Linux Runtime），一次性资源归零。

| 资源 | 处置 |
| --- | --- |
| `ccrun` 账号、home、rustup/Node、36 个系统包 | **保留**（撤销脚本仍在：`p12-provision-linux-runtime.sh --revert`、`p12-runtime-toolchain.sh --revert`） |
| 两把一次性 SSH 密钥（客户端一把、fodelf 一把） | 私钥已删（两把都没离开过所在机器），两行公钥按 root 清单精确从 `ccrun` 的 `authorized_keys` 移除，删完 **0 行**——所以现在没有任何入站 ssh 能进那个账号，要再用它得先装一把新的 |
| 两端 `~/.ssh/config` 的标记块 | 已删；删后与开工前的备份逐字节相同（`diff` 无输出），备份随即删除，两端分别回到 77 行和 32 行 |
| 三个一次性 workspace 与 Runtime 上的 `config.toml` | 已删（那份配置里只有本轮的 workspace） |
| fodelf 的 `~/ccnm-p12/`（MCP 配置与 prompt） | 已删；`~/.claude.json` 里没有本轮的 project 条目 |
| 项目副本 `~/projects/ccnm`、readonly/closed 两棵空树、bundle、`~/.local/state/ccnm` | 已删 |
| 部署的二进制 | 两端保留新的，可回退副本也保留（fodelf `ccnm.pre-p12`、Runtime `ccnm.pre-p12fix`） |
| root 清单 `/var/lib/ccnm-p12-20260911` | **保留**——它记着"账号和这 36 个包是本轮装的"，将来真要归零时 `--revert` 得靠它。里面另加了一个 `keys-removed` 说明钥匙这一半已经撤了 |

逐条只读复核见[会话计划](../plan/p12-real-project-session.md)第五节与 [status.json](../plan/status.json) 的 P12 evidence。

## 八、限制

- **一台 Linux、一种发行版、一个 Host。** Debian 13 / x86_64 / Claude Code 2.1.268 的 `-p` 打印模式。没验 Codex，没验交互式 UI，没验 arm64 Linux 或非 Debian 系。
- **项目是 ccnm 自己。** 一棵 Rust + Python 的中型树，有 git 历史、有真实测试。不代表 monorepo、不代表 node_modules 那种量级，也不代表需要 C++ 或 GPU 的项目（`g++` 本轮故意没装）。
- **Node 只验到"叫得动"。** `node`/`npm` 的版本是从 `exec_command` 里问出来的，但没有在这一轮跑一个真实 node 项目的 `npm ci && npm test`。
- **bridge 启动失败对 Host 用户仍然不可见**（P11 记过的那条）：Claude Code 只显示 `Connection closed`，`CCNM_E_*` 留在 Host 丢掉的那条 stderr 上。本轮再次确认，ccnm 单方面改不掉。
- **egress 不作保证**，见第六节。
