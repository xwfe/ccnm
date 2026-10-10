# 运维：装、退、清、修

装上去之后要做的事。每条都是能照着敲的命令，不是原则说明。角色和 topology 见[架构说明](architecture.md)，能力边界见[支持矩阵](support-matrix.md)。

## 安装与升级

两台机器要装**同一个 build**。版本或构建不同时，doctor 报失败，起会话也会被拒（`CCNM_E_VERSION`），报法见下面的表；版本号一样、构建不同的情况见[排错手册](troubleshooting.md#doctor-报-reports-ccnm-版本-like-this-machine-but-it-is-not-the-same-build)。

### 用发布包升级（一般就用这个）

**要换的是每一份会被执行的 ccnm**，不只是你敲命令的那份。以 0.13.1 升 0.14.0 为例，漏了哪份 doctor 怎么报：

| 哪台 | 哪个账号 | 漏了会怎样 |
| --- | --- | --- |
| 放项目的机器（Runtime） | 你敲 `ccnm` 的账号 | `Agent 的 ccnm` 行失败：`the Agent Node <名字> runs ccnm 0.14.0, this machine runs 0.13.1; install the same build on both` |
| 放项目的机器（Runtime） | 执行账号（AI 那台 ssh 登进来的那个账号：默认就是你自己的，另建了专用账号就是它，比如 `ccrun`）。AI 那台经 ssh 调起的是它名下 `ccnm_bin` 指的那份，默认 `~/.local/bin/ccnm` | `反向 SSH` 行失败：`the Runtime Node runs ccnm 0.13.1, this machine runs 0.14.0; …`；起会话也被拒，报同一句外加 `before starting a session` |
| 跑 AI 的机器（Agent） | 跑 Controller 的账号 | `Agent 的 ccnm` 行失败：`the Agent Node <名字> runs ccnm 0.13.1, this machine runs 0.14.0; …`。换了文件没重启 Controller，见第 4 步 |

一台机器同时当两个角色、或执行账号就是你自己，就少换几份。顺序：

**1. 停掉所有会话。** 在放项目的机器上：

```bash
ccnm status                                  # 不带项目名：所有项目，连同本机的 mcp-serve 进程
ccnm stop <workspace>                        # 上面列出来在跑的，每个都停
ps aux | grep '[c]cnm internal mcp-serve'    # 应该什么都不打
```

**不停会怎样**：正在跑的会话不会被升级杀掉，但它连着的 `ccnm internal mcp-serve` 还在用老代码跑，而且攥着这棵工作树的写锁。升完之后新会话起不来（doctor 的 `远端 MCP 握手` 报 `workspace write guard is busy`），会话里模型手里一个工具都没有，会把工具调用**当成普通文本打出来**，看着像模型抽风。已经升完才想起来，按[写入 guard 残留](#写入-guard-残留)收。

**2. 每台下载自己系统的包，核 sha256。** Mac 用 `macos-universal`，Linux 用 `linux-x86_64`：

```bash
v=<版本号>; p=macos-universal                # 版本号看 Releases 页；Linux 上 p=linux-x86_64
base=https://github.com/xwfe/ccnm/releases/download/v$v
curl -fLO $base/ccnm-$v-$p.tar.gz && curl -fLO $base/ccnm-$v-$p.tar.gz.sha256
shasum -a 256 -c ccnm-$v-$p.tar.gz.sha256    # Linux 上用 sha256sum -c；要看到 OK
```

**3. 新文件 + 改名放进去**，别 `cp` 盖（[原因](#千万不要-cp-覆盖正在用的二进制)）：

```bash
tar -xzf ccnm-$v-$p.tar.gz                   # 包里只有一个 ccnm
install -m 755 ccnm ~/.local/bin/ccnm.new && mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
~/.local/bin/ccnm --version                  # 要打出新版本号
```

执行账号就是你自己的账号时，照上面做就行。另建了专用执行账号（比如 `ccrun`）时，它通常不能从你的账号直接 ssh 进去：用有权限的账号把包放过去、`chown` 给它，再 `su - ccrun` 在它名下做第 2、3 步。

**4. 跑 AI 的机器上重启 Controller：**

```bash
ccnm controller install
```

不重启的话，Controller 还是那个用旧文件起的进程。Mac 上它会换掉 launchd 里的那个；Linux 上是 `systemctl --user` 的 daemon-reload、enable、restart，**要在这个账号用 ssh 登录进来的会话里跑**（`su` 进来的报 `Failed to connect to bus`，见[排错手册](troubleshooting.md#linux-上-ccnm-controller-install-报-failed-to-connect-to-bus)）。重启 Controller 不会断已有的会话，但第 1 步已经都停了。最后一行 `listening: ccnm <新版本号> as <账号>, pid …` 的版本号要是新的；还是旧的，见[下面](#升级完-controller-还是旧的)。

**5. 在放项目的机器上跑 doctor：**

```bash
ccnm doctor <workspace>
```

最后一行是"可以用了"就是换好了。`Controller` 行写的是正在应答的那个进程的版本，它不跟别的比——这一行还是旧版本号，就是第 4 步没生效。

出问题就[回退](#回退)：把上一版的包按同样的步骤装回去。

### 从源码部署（开发用）

先照上面第 1 步停掉所有会话，再：

```bash
bash scripts/deploy.sh <另一台的 ssh 别名> [workspace]
```

在有 Rust toolchain 的那台上跑。它编译、装两边、重启 controller、最后跑一次 `ccnm doctor`。它只装两台各自登录账号的那份，执行账号的那份要自己按上面第 2、3 步换。Agent 是 Linux 时它也会去重启 systemd 里的 Controller，但这条路还没在真机上跑过。

### 升级完 Controller 还是旧的

`ccnm controller install` 最后一行报的是**此刻在 socket 上应答的那个进程**。同一个 socket 上要是还有一个不归 launchd / systemd 管的 Controller（手工起的，或者很老的构建留下的），install 换不掉它，报出来的就是它；doctor 的 `Controller` 行只报版本号，也分不出同号的新旧构建。

核对：二进制的修改时间要早于 Controller 进程的启动时间。

```bash
ssh <agent> 'stat -f "%N %Sm" ~/.local/bin/ccnm; ccnm controller status'   # Agent 是 Linux 时用 stat -c "%n %y"
ssh <agent> 'ps -o pid=,lstart=,command= -p <上面那个 pid>'
```

进程比二进制还老，或者 `controller status` 写着 `not under systemd (started by hand)`，就先卸再装，再按 pid 确认旧进程退了（卸载不保证它退出，没退就自己结束它）：

```bash
ssh <agent> 'ccnm controller uninstall && ccnm controller install'
```

### 千万不要 `cp` 覆盖正在用的二进制

升级第 3 步和 `deploy.sh` 都用"新文件 + 改名"，原因就在这里。在 Apple Silicon 上，往一个已经执行过的 Mach-O 里写东西会让它的代码签名失效，之后每一次 exec 都直接 SIGKILL（退出码 137），而**已经在跑的那个进程照常用旧代码继续**。

症状极具迷惑性：`ccnm --version` 显示 `Killed: 9`，`doctor` 报空回复，而 `launchctl` 坚称 controller 一切正常。

正确做法是写一个新文件再 rename 覆盖——inode 变了，运行中的进程留着自己那份，下一次 exec 拿到完整且签名正确的新文件：

```bash
install -m 755 target/release/ccnm ~/.local/bin/ccnm.new
mv ~/.local/bin/ccnm.new ~/.local/bin/ccnm
```

远端同理，而且 `scp` 要带 `-p` 并显式 `chmod +x`：OpenSSH 10.3 的 scp 不带 `-p` 会丢掉权限位，10.2 的会保留。落一个 0644 的二进制过去，对面报的是 `zsh: permission denied`，看着像安装坏了而不是少了一个执行位。

### 回退

回退就是把旧版本按同样的方式装回去，没有单独的回退命令，也不用事先留备份。用发布包的，把[升级那几步](#用发布包升级一般就用这个)里的 `v=` 换成上一版，从第 1 步走一遍（Releases 页每个版本的包都留着）。

从源码部署的：

```bash
git checkout <旧的 tag 或 commit>
bash scripts/deploy.sh <另一台的 ssh 别名>
```

两边必须一起退。只退一边的话，下一次 `ccnm doctor` 会把它标成 FAIL，起会话也会被拒（`CCNM_E_VERSION`）。退完一定重启 controller（`deploy.sh` 会做），已有会话不受影响。

**状态文件不随回退变化。** 会话记录里带着写它时的协议号：协议号没变，旧版本会忽略不认识的字段照常读；协议号变了则明确拒绝，而不是当成半懂的记录接着用。

保险起见，回退前把正在跑的会话停掉——一个会话的两半分别记在两台机器上，让它跨越一次两端不同步的回退没有意义。

## 配置迁移：legacy → Agent Instance

**什么时候需要**：要用 Codex、一台 Agent Node 上配多个实例、或者要用 `--agent` 切换时。只用 Claude、用默认登录的，不用改。

**没有自动迁移命令**，是手动改配置文件。改动很小，改完用 `doctor` 验。

Runtime Node 的 `~/.config/ccnm/config.toml`，把 workspace 的 `agent_node` 换成 `agent`：

```toml
# 之前
[workspaces.demo]
agent_node = "worker"
root = "/Users/me/demo"

# 之后
[workspaces.demo]
agent = { node = "worker", instance = "claude-main" }
root = "/Users/me/demo"
```

Agent Node 的配置里定义那个 instance：

```toml
[agents.claude-main]
provider = "claude"
profile_ref = "default"
```

然后在 Runtime Node 上验：

```bash
ccnm doctor demo
```

三条容易踩的：

- **两个字段不能同时存在。** 配置校验会拒绝 `agent` 和 `agent_node` 并存，报 `cannot combine agent with legacy agent_node`。
- **instance workspace 的 root 只能定义在它的 Runtime Node 上**，在 Agent Node 的配置里写同名 workspace 会被拒。
- **instance workspace 改不了权限模式**，Claude 固定用 `acceptEdits`：`claude_permission_mode` 只写 `"acceptEdits"` 不报错，写别的值被拒。

迁移前有会话在跑的话，先 `ccnm stop <workspace>`：会话记着自己的 identity，配置换了它也不会跟着换。

## 项目目录：属主要对，git 身份要配

真机上撞出来的两条，装完环境第一次放真项目时一定会遇到。

这里说的“执行身份”是 **Runtime Executor**：Agent 的 MCP transport 落到的那个账号，项目工具真正以它的身份跑。敲 `ccnm` 的是 **Operator**，通常是你自己的账号。默认两者可以是同一个账号；另建了专用账号（通常叫 `ccrun`）就是两个，下面说的情况多半出在这时——四种身份的完整边界见[生产安全](production-safety.md)。

**项目目录必须属于 Runtime 执行身份本人，光可写不够。** 放在别人拥有的 0777 目录里，文件是写得进去，但那个身份跑任何 git 命令都会被拒：

```text
fatal: detected dubious ownership in repository at '/path/to/worktree'
```

`ccnm doctor` 的 `workspace 根目录` 那一行连属主和 git 一起查：git 因属主拒绝时这一行是 FAIL 并直说原因，属主不对但 git 能用时是 WARN。绿灯这才等于"这个身份真的能用这个项目"。

**项目可以放在执行身份自己的家目录里，哪怕 Operator 进不去**。Debian 12 起新账号的家默认 0700（`/etc/login.defs` 的 `HOME_MODE`），Operator 看不了 `/home/ccrun/` 下的任何东西；macOS 的家目录默认别人能进入，所以只有 Linux 会遇到。ccnm 把"不在"和"这个账号没权限看"分开处理：

- `ccnm workspace add <名字> /home/ccrun/<项目>` 照常登记，并提示它没能核对、也没解析符号链接。**写绝对路径**，和执行身份自己 `pwd -P` 看到的一致。
- `ccnm run` 不拦；项目在不在由执行身份在开会话时回答。
- `ccnm doctor` 的 `Runtime 上的项目` 一行是"不查"（不挡结论），执行身份的回答在 `workspace 根目录` 那一行。

**新建的执行身份没有 git 身份，第一次 commit 直接失败：**

```text
fatal: unable to auto-detect email address
```

`doctor` 不查这个。在那个身份下配一次就行：

```bash
git config --global user.name "<name>"
git config --global user.email "<email>"
```

不配也能干活——Agent 会退而用 `git -c user.name=… -c user.email=…` 传单次参数，但**下一个会话还会撞同一堵墙**。（[P12 那一轮](research/p12-real-project-2026-09-11.md)里真实 Claude Code 就是这么干的：它没去改那台机器的 git 全局配置，只给自己那一次提交带上了身份。）

### 专用账号写进项目的配置，你打开时按你的身份生效

专用账号模式下（写了 `runtime_user`），`ccnm doctor` 有一行 `以你身份生效的文件`（`Files that act as you`）。项目里有 `.vscode`、`.idea`、`.claude`、`.codex`、`.mcp.json`、`.gitmodules`、`.gitconfig`、`.ripgreprc` 或 shell 启动文件时，这一行是"注意"，并把它们列出来；一个都没有时是"正常"。这一行不挡结论。

原因：这些是 IDE、Claude Code、Codex 打开项目时会读来执行的配置。VS Code 的任务、Claude Code 的 hooks、`.mcp.json` 里的 server，都按**打开它的人**的权限跑。执行账号写进去的东西，等你本人打开这棵树时就以你的身份执行了，专用账号隔出来的那道边界就这么绕过去了。

**怎么做：**

- **以你自己的身份打开这棵树之前**（IDE、Claude Code、Codex），先把这一行列出的文件看一遍。
- **别给这棵树加 git 的 `safe.directory`。** 执行账号不是你本人时，你的 git 因属主不同拒绝它（`detected dubious ownership`），所以它的钩子和 `core.fsmonitor` 不会以你的身份跑。加了，执行账号写进 `.git/hooks` 的东西就会在你跑 `git status` 时执行。`.git` 因此不在这一行的清单里。
- **要硬挡**（Linux，Debian 13 实测）：由 root 递归设不可变标志。还不存在的条目执行账号能新建，所以要挡的话由 root 先建出空目录再设：

  ```bash
  sudo mkdir -p <项目>/.vscode <项目>/.claude
  sudo chattr -R +i <项目>/.vscode <项目>/.claude
  ```

  设完后，执行账号对这两个目录里的东西改不了、删不了、挪不走，也撤不掉这个标志。**一定要带 `-R`**：只给目录本身设，挡住的只是在目录里新建、删除、改名，目录里已有的文件照样能改（实测 `.vscode/tasks.json` 被改写成功）。代价是 git 要改这些文件时（切分支、pull）会失败（按标志的语义推断，没实测）。撤掉用 `sudo chattr -R -i <同样的路径>`。macOS 上对应的是 `chflags`，没实测，这里不写做法。

**为什么不用 chmod 或 ACL**：项目目录必须归执行账号所有（见本节开头），属主自己就能改权限和 ACL，还能把整个条目挪走再建一个新的。同样的原因，doctor 不查"写不写得了"：这个模式下答案永远是"写得了"。

**`runtime_user` 写的就是你自己的账号时**，这一行照样会出，但它提醒的事不存在：执行账号就是你，写进去的东西本来就以你的身份跑。想让它消失，就删掉 `runtime_user`（连同只在专用账号模式下有用的几个 `allow_*`），改用共用账号模式。

**共用账号模式（没写 `runtime_user`）没有这一行**：命令本来就以你的身份跑，这些文件不额外给它什么。

## Runtime Node 的前置条件与项目工具链

**ccnm 不装工具链，不升级它，也不代管版本。** 它不知道你的项目要什么。装什么、装在哪、谁维护，是 Runtime Node 管理员的事——这一节说的是怎么装得让工具**真的能被调用到**，因为这里有一脚很容易踩空。

**ccnm 自己要两个程序**，两个入口都要：

| 程序 | 谁要它 | 没有它会怎样 |
| --- | --- | --- |
| `git` | `list_files`、写 guard 的资源判定、项目自己 | 降级成非 git 视图；guard 按目录而不是按仓库互斥 |
| `ripgrep`（`rg`） | `search_text`——它不自己扫文件；doctor 不查它 | 少了 `search_text`，模型一搜就报 `ripgrep is not installed on the Runtime Node` |

workspace 开了 [`exec_sandbox = "codex"`](configuration.md#exec_sandbox)（`exec_command` 包进 Codex 的 OS 沙箱）时，Runtime 上还要：

| 前提 | 为什么 | 没有它会怎样 |
| --- | --- | --- |
| 节点配置里的 `codex_bin` 指向 Codex 0.154.0 | 沙箱是它的 `codex sandbox` | 会话启动前报 `CCNM_E_CONFIG` 或 `CCNM_E_VERSION`，任何入口的会话都起不来，不会退回裸跑 |
| **Linux**：装 `bubblewrap`，并允许执行账号创建 user namespace（Debian 13 默认允许） | Codex 在 Linux 上用 bwrap 实现 workspace-write 沙箱 | 会话启动时那次 `sh -c 'exit 0'` 探测失败，报 `CCNM_E_DEPENDENCY`（带 Codex 自己的报错），不会退回裸跑；doctor 不提前查（[配置说明](configuration.md#exec_sandbox)） |

Codex 的 Linux 沙箱会在真实的 `/tmp` 里留下几个空目录（`/tmp/.git`、`/tmp/.agents`、`/tmp/.codex`、`/tmp/codex-bwrap-synthetic-mount-targets-<uid>/`），属主是执行账号，用完不删；`/tmp` 是 tmpfs 的话重启就没了。这是 Codex 的行为，ccnm 不清理它们。

剩下的是项目自己的：编译器、包管理器、测试运行器。

**装在执行身份自己的 home 里，不要装成全机共享。** 这不是洁癖：Runtime Executor 的意义就是"除了这个项目什么都没有"，而一个装到 `/usr/local` 的工具链会同时属于机器上每个账号。[P12 那一轮](research/p12-real-project-2026-09-11.md)在 Debian 上的做法是：

- 系统级只装 Rust 链接期要的 C 工具链（`gcc libc6-dev make`）和 `ripgrep`——rustc 自己不带 linker，这一步绕不开；
- rustup（`~/.rustup`、`~/.cargo`）和官方 Node 二进制包（`~/.local/node-<版本>`）以**那个账号自己的身份**装进它的 home；
- 两条命令写成了可重跑、可撤销的脚本：[建执行身份](../scripts/p12-provision-linux-runtime.sh)（要 root，清单先于变更、`--revert` 按清单精确撤销）和[装工具链](../scripts/p12-runtime-toolchain.sh)（**不要 root**）。它们是那一轮的实测做法，可以照抄，也可以只当参考。

### 最容易踩的一脚：非交互 ssh 的 PATH

`exec_command` 的命令跑在一条**非交互** ssh 会话里，而大多数工具链安装器写的 PATH 在那条会话里不生效：

- Debian/Ubuntu 的 `~/.bashrc` 第 6 行就是 `case $- in *i*) ;; *) return;; esac`，非交互直接返回；
- rustup 默认把 PATH **追加在文件末尾**（也就是那个 `return` 之后），另一份写在只有 login shell 才读的 `~/.profile`。

照默认装完，ccnm 报的是

```text
cargo is not installed on the Runtime Node, or is not on its PATH
```

**看着像没装，其实是装了但 PATH 没到。** 做法是把 PATH 那一块写在那个 `return` **之前**（rustup 用 `--no-modify-path`，自己写），然后从客户端问一次——只有这一句话算数：

```bash
ssh <runtime-alias> 'command -v cargo node npm rg git'
```

在 Runtime 上 `echo $PATH` 不算：那是登录 shell 的答案，不是 `exec_command` 会看到的那一条。

### 装工具链需要出站网络

工具链要从网上下载，所以**装的时候** Runtime 得出得去；`exec_command` 之后能不能出去是另一个问题，ccnm 对此[不作保证](support-matrix.md#egress不作保证)。真机上还撞到过出口不均质：`static.rust-lang.org` 直连没问题，`nodejs.org` 会 TLS reset（`curl: (35) Recv failure`）。换镜像可以，但**完整性要用官方哈希校验**——在能连上官方的那台机器上取 `SHASUMS256.txt`，把哈希带过去固定，不要信镜像自己给的清单。

## 状态文件在哪，多大，怎么清

两边都在 `${XDG_STATE_HOME:-~/.local/state}/ccnm/`，但内容分工不同。

**Agent Node：**

```text
sessions/<ccnm-session-id>/
├── session.json     启动这个会话需要的全部信息
├── mcp.json         给官方 CLI 的 --mcp-config：通往 Runtime 的那一条 ssh
├── settings.json    --settings：Runtime 工具及按配置允许的 Agent 能力
├── stdout           官方 CLI 的 stdout（print 模式下是 JSON 结果）
├── stderr           官方 CLI 的 stderr
├── supervisor.log   supervisor 自己的诊断
├── tmux.conf        ccnm 自己那个 tmux server 启动时读的配置（见下）
└── exit             最后写的：它是怎么结束的
workspaces/<name>/   官方 CLI 的工作目录
controller.sock      controller 的监听 socket
```

**Runtime Node：**

```text
sessions/<ccnm-session-id>/output/   exec_command 留下的命令输出（会自己清，见下）       执行账号的
write-guards/                        工作树级独占锁                                       执行账号的
rpc/sessions/<handle>.json           machine API 的会话记录                               Operator 的
rpc/start-keys/                      启动幂等键                                           Operator 的
rpc/outputs/<handle>/                session.result 从 Agent 拷来的输出                   Operator 的
ssh/                                 ControlPath socket
```

最后一列是"在谁的 state 目录里"。另建了专用执行账号时，Operator（敲 `ccnm`、跑 `ccnm rpc` 的账号）和 Runtime 执行账号（Agent 的 ssh 落到的账号，通常是 `ccrun`）是两个账号，这两组东西在两个不同的目录里，谁的东西只能由谁删；默认的共用账号下它们是同一个目录。

`tmux.conf` 写在会话目录里，是因为那是 ccnm 一定拥有、一定存在的目录。tmux **只在启动 server 的那一刻**读它，所以哪个会话的那份起的作用不重要，跟着会话一起被删也不影响任何东西。里面设了什么、怎么改回去，见[使用说明](usage.md#会话在-tmux-里所以滚屏和复制跟你平时不一样)。

**Agent 上的会话记录没有自动清理，也没有保留期**，一直留着，除非你删。这是刻意的：一个已经结束的会话，它的输出往往比它本身有价值。单个会话通常几十 KB。

**Runtime 上的 `output/` 例外，它自己清。**这是 `exec_command` 留下的完整输出，大小看命令打印了多少。规则（数字的出处和并发时的细节见[协议第 8 节](protocol/remote-workspace-mcp-v1.md#8-输出预算与保留)）：

- 一个会话最多留 256 MiB 左右，满了从最旧的运行删。
- 外部 MCP（`ccnm mcp bridge`）的会话，连接一断就删。前提是 `mcp-serve` 自己正常退出：它被 `kill -9` 时后台命令会继续跑，输出留在 `sessions/bridge-<id>/output/`，`ccnm cleanup` 按设计不列 bridge 会话，只能等下面的 7 天过期或由执行账号手动删（先按[写入 guard 残留](#写入-guard-残留)收掉还在跑的命令）。
- Managed 会话断开不删，`/mcp Reconnect` 回来还要读。它的输出在**最后一次运行过去 7 天、且这台机器上没有 `mcp-serve` 在服务它**之后删。
- 过期检查在执行账号每次起 `mcp-serve` 时做（任何会话都算，包括 `ccnm doctor` 的握手），在后台跑，不拖慢连接。`ps` 跑不了时一个都不删。

所以一台 Runtime 上 `output/` 的总量最多大约是"最近 7 天里跑过命令的 Managed 会话数 × 256 MiB"。

**Machine API 的输出占多少盘、在哪**：Agent 在会话目录里为读过的流各存一份只读视图（`sessions/<id>/stdout.view` 等，每个流最多约 32 MiB，全是非法 UTF-8 的极端情况最多约 96 MiB）；Runtime 这边第一次 `session.result` 时整份拷到 `${XDG_STATE_HOME:-~/.local/state}/ccnm/rpc/outputs/<session>/`。两边都不会自动删；要腾地方用 [`ccnm cleanup`](#想立刻腾地方ccnm-cleanup)，它先预览，由各自的账号删，Machine API 的记录留作墓碑。

### 想立刻腾地方：`ccnm cleanup`

在 Runtime Node 上用 Operator 账号（平时敲 `ccnm` 的那个）：

```bash
ccnm cleanup demo                          # 只列清单，什么都不删
ccnm cleanup demo --apply <预览打印的令牌>   # 照清单删
```

预览逐项列出三个账号各自为这个 workspace 留下的东西：Agent 的会话记录，Runtime 执行账号的 `exec_command` 输出，Operator 自己的 Machine API 结果拷贝。每一项都写明属于哪个 uid、多大，打算删还是留，留的话为什么。确认后把最后一行命令原样执行一遍。

- **令牌 15 分钟有效。** apply 时会重新向三方各收一遍清单，只要有任何变化（有会话跑了、文件写了、配置改了），就整份拒绝、一个都不删，报 `no longer what was previewed`（退出码 3），重新预览即可。令牌不是秘密，只是用来确认"删的就是你看过的那份"。
- **各删各的。** Operator 只删自己的 `rpc/outputs/`；Agent 的会话记录由 Agent 删；Runtime 的输出由 Agent 经它到 Runtime 的那条 ssh 请执行账号删。不需要 root，也不需要 Operator 能读执行账号的目录。
- **会留下的**：还没结束、结没结束说不清、正在被用（有 `mcp-serve` 在服务、命令还在跑、会话控制锁被占）、写锁标着的会话（恢复要用它的记录，见[写入 guard 残留](#写入-guard-残留)），以及不是 ccnm 建的普通目录（比如符号链接）或属于别的账号的东西。`ps` 跑不了时 Runtime 那边一律不删。Runtime 那一半没删掉的会话，Agent 上的记录也先留着：Runtime 的输出不记属于哪个 workspace，以后要找它，只能靠 Agent 上这条记录。
- **Machine API 的记录不删，只清输出。** `rpc/sessions/<handle>.json` 里大的部分（最终回答、尾巴）清掉，其余留作墓碑：`session.status` 照常回答，`session.result` 回 `-32012`（`reason: cleaned`），同一个 `start_key` 仍指回原会话、**不会重跑**。`rpc/start-keys/` 永远不删。
- **不会碰**：项目本身、写锁、登录凭据、别的 workspace。
- **没做完怎么办**：预览里说要删的，有一项没删成（输出里是 `FAILED`，或者当场发现在用、变了而留下），或者有一方问不到，退出码就是 3；预览时本来就说要留的不算。已删的不会恢复，也不会重删；再预览一次就能对剩下的重试。

**不是恢复工具**：它不杀进程、不动写锁。写锁标着的会话要先按[写入 guard 残留](#写入-guard-残留)处理完，再来清。

要连 workspace 一起清：

```bash
ccnm workspace remove demo --purge     # 先停会话，再清 ccnm 为它保存的东西，最后从配置里去掉
```

`--purge` 走的是同一个清理服务，`--purge` 本身就算确认，不再要令牌；另外还会删 Agent 上这个 workspace 的 CLI 工作目录。区别在最后一步：**只要有任何东西没清掉（包括上面"会留下的"），workspace 就留在配置里、退出码 3**，因为配置是以后唯一还能找到那些东西的入口。处理完再跑一次；只想忘掉 workspace、数据留着不管，去掉 `--purge`。

## 停止

```bash
ccnm stop demo                                   # 停这个 workspace 的会话
ccnm stop demo --agent codex-main --session <id>  # 精确停一个
```

停成功的判据是**三件事都被观察到**：Agent 进程组结束、承载工具调用的 MCP transport 结束、Runtime 的写入 guard 释放。任何一条证明不了，状态停在 unknown 而不是报成功——写权限提前交给下一个人，两个 Agent 就会同时改一棵工作树。

`ccnm status demo` 看当前状态。两个实测出来的坑：

- **`stop` 对已经结束的会话是幂等的**：没有会话在跑时它退出码 **0**，报告里 `killed` 为 false。清理脚本可以无脑调一次，不用先判断有没有人在用。

  但幂等**不等于 stop 永远不报错**。有一种情况仍然是失败：workspace 的终端**确实在跑**，而 ccnm 认不出它是不是你选的那个会话——报 `CCNM_E_NOT_READY: a terminal is running for this workspace but carries no verifiable ccnm session identity`。那道检查是为了不去杀别人的会话，跟幂等无关。

  另外，`--session <id>` 指到一个这台机器上没有记录的 id，仍然报 `no session <id> on this machine`：不知道那个会话，和知道它已经结束，是两件事。

- **`status` 的会话列表看不见 `ccnm run --print` 的会话。** 它只报 Agent Node 上的 tmux 会话，非交互的 print 运行不在其中——会话正跑着，`status` 照样说 `no live sessions`。要判断有没有人在写，看同一条命令输出最后的**写锁**那一行（在 Runtime Node 上跑 `ccnm status <workspace>` 才有），它由 Runtime 执行账号自己回答，print 运行和外部 MCP 客户端占着锁都看得见：

  ```text
  写锁  被占：会话 402638ca 正持有（09-29 13:40）
         pid 4242 还是 ccnm 进程；Agent 那边这个会话：运行中
         标记文件：Runtime 执行账号的 write-guards/00aa11bb22cc33dd.lock
  ```

  它只是看一眼：不建文件、不改标记、不拿写权。"空闲"是那一刻的样子，不是替你占住；"问不到 Runtime"不等于空闲。为什么要绕 Agent 去问：写锁在执行账号自己的 state 目录里，Operator 账号通常读不到，读自己的 `write-guards/` 看到的是另一个写域（见[一棵树配两个 state 目录](#一棵树配两个-state-目录--两个互不知晓的写域)）。

## 故障恢复

### 写入 guard 残留

症状：新会话起不来，报工作树被占，但没有会话在跑。Machine API 的 `session.start` 这时回 `-32007`，`data.reason` 是 `left_held` 或 `kept_on_purpose`。

先在 Runtime Node 上跑 `ccnm status <workspace>`，看最后的写锁行。它告诉你是哪一种、标记文件叫什么、标记里的 pid 现在是什么：

| 写锁行说 | 意思 | 往下看 |
| --- | --- | --- |
| 被占 | 有进程正持有。等它结束，或者去结束它 | Agent 那边说这个会话已经结束的，是孤儿 `mcp-serve`，看下面"孤儿 `mcp-serve` 占着锁" |
| 故意留着 | 上一个会话有东西停不掉 | 下面"有 `abandoned` 这一行" |
| 说不清：标记说……占着，但没有进程持锁 | 异常退出留下的 | 下面"没有第二行" |
| 说不清：标记内容不完整 / 读不了 | 标记损坏，或执行账号读不了自己的目录 | 按"没有第二行"的顺序处理；读不了的先查目录属主和权限 |

这一行只是看，不清理任何东西；下面的恢复仍然要人按顺序做。

#### 孤儿 `mcp-serve` 占着锁

Agent 那边的会话早结束了，Runtime 上它的 `mcp-serve` 还活着（典型是 Runtime 笔记本睡眠时连接成了半开）。它空闲时每 30 秒 ping 一次，一般半分钟内自己就退了。没退的话：

**别直接 kill `mcp-serve`**，那会留下 `held` 标记，还得再删 marker。结束它背后那个 sshd 会话，`mcp-serve` 读到 EOF 会正常收尾、锁变 `released`，不用动 marker：

```bash
ps -o pid,ppid,lstart,command -p <mcp-serve 的 pid>   # PPID 那列是 sshd-session
kill <那个 sshd-session 的 pid>
```

动手前先确认 Agent Node 上那个会话确实结束了（会话目录里有 `exit` 文件，没有对应的 `ccnm internal supervise` 进程）。

#### 标记文件长什么样

Runtime 的 `write-guards/` 里那个 marker 长这样：

```text
held <session> <workspace> pid <pid>
```

**先看有没有第二行**，两种情况的处理不一样：

```text
held bridge-abandoned demo pid 25669
abandoned 1 command(s) (r-e69acf4e804643a2)
```

**有 `abandoned` 这一行 = 不是异常退出。**上一个会话结束时有命令停不掉（macOS 上离开了进程组、又攥着管道那种，ccnm 的信号够不着它；Linux 上这种会被收掉，杀不掉的才留在这里，写成 `abandoned 1 process(es) that left their process group (pid 4321 (sleep 120))` 这样），ccnm 明知有东西可能还在改这棵树，**故意**没把写权交出去。所以**先去收那些命令，别急着删 marker**——删了就是放第二个写者进同一棵树，那正是这把锁存在的理由。每条命令的命令行在 `${XDG_STATE_HOME:-~/.local/state}/ccnm/sessions/<session>/output/<ref>/status` 里；进程要按它自己留下的进程组找，`ccnm status` 看不到它们。`ccnm status` 这时会说"故意留着的"，不是"异常退出留下的"。

第二行也可能是 `abandoned MCP server <名字> (process group <组号>: <pid>, ... still running after SIGKILL)`，或 `... could not be checked: ...`：`call_mcp_tool` 转接的 server 关掉后，它进程组里还有 SIGKILL 也杀不掉的进程（setuid 程序、卡在内核里的），或者 ccnm 跑不了 `/bin/ps` 没法确认。用 `ps -A -o pid,pgid,stat,command` 按组号找，那几个 pid 都结束了再往下删 marker。一直是"查不了"的，先看这台机器有没有 `ps`（精简 Linux 镜像要装 procps）。

**没有第二行 = 异常退出留下的**，状态是 unknown。**ccnm 不会因为时间过去就自动接管**——它证明不了旧的执行者已经结束。marker 里的 pid 只帮你少找一步：拒绝信息会告诉你那个 pid 现在是什么（还在跑，连命令行一起给你；已经不在；或者被别的程序复用了）。**pid 没了不等于可以接管**——它起的命令可能还活着，而这里看不见它们。

恢复必须由 Runtime 操作者做，顺序不能反：

1. 先证明旧的都结束了：`ccnm status <workspace>` 加进程列表，确认旧 supervisor、Agent、SSH MCP 及其子进程都没了。marker 里有 pid 的话先查它（`ps -o pid=,lstart=,command= -p <pid>`）。
2. 在 `${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/` 里找到包含那个 session id 的**单个** marker 文件。
3. 备份后删掉那**一个**文件。

不要批量删，不要仅因为"过了很久"就清。**证明不了旧执行者结束时，保持 unknown 才是对的状态。**

#### 一棵树配两个 state 目录 = 两个互不知晓的写域

写锁存在**传给 ccnm 的那个 state 目录**里（`${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/`），文件名按工作树的规范化路径算。所以同一棵工作树，只要两边的 `XDG_STATE_HOME` 不一样，就是两把互不相干的锁：两个 coding 会话能同时开起来，各写各的，谁也不知道谁。2026-09-20 用真实二进制实测过——两个会话都成功写进了同一棵树（[P43 记录](research/p43-guard-recovery-2026-09-20.md)）。

这是设计的边界，不是 bug：ccnm 不往工作树里放状态，也不占用系统级的固定路径。避开它只有一条：**同一台机器上服务同一棵树的所有 ccnm 进程，用同一个 `XDG_STATE_HOME`**。两个不同的系统用户各自跑 ccnm 服务同一棵树也是这个问题（各自的 home 就是各自的 state），那种情况下这把锁保护不了你，得靠别的办法（比如干脆不让第二个账号写那棵树）。

### controller 不响应

```bash
ccnm controller status      # 在监听吗？是怎么跑起来的？
ccnm controller install     # 重装并重启；已有会话不受影响
```

**Linux 上**它是 systemd 用户服务 `dev.ccnm.controller.service`：`systemctl --user status dev.ccnm.controller.service` 看状态，日志在 `~/.local/state/ccnm/controller.log`（和 macOS 同一个文件）。`controller install` 就是重写单元文件再 `systemctl --user restart`；单元里写了 `KillMode=process`，重启只换 Controller 本身，它起的 tmux 和会话照常跑。`status` 那一行写 `systemd user service` 才是被 systemd 管着的；写 `started by hand` 说明它是手工起的，退出登录、重启机器都不会自己回来。linger 关着时 `status` 会提示，原因与开法见[快速开始](getting-started.md#4-初始化跑-ai-的机器)。

**macOS 上**，`managername` 必须是 `Aqua`。是 `Background` 时见[排错手册](troubleshooting.md#doctor-的-controller-行失败写着-backgroundmacos)；`ccnm run` 会在创建会话前就拒绝，报 `CCNM_E_NOT_READY`。

**配置或状态目录不在默认位置时**（用了 `--config` / `CCNM_CONFIG`、`XDG_CONFIG_HOME` 或 `XDG_STATE_HOME`），先 `ccnm controller install --dry-run` 看一眼：这几个变量会写进 plist，安装计划里每个一行 `with 变量=值`，`--config` 给的相对路径会换成绝对路径（launchd 在 `/` 下启动 Controller）。

一个账号只有一个 Controller：Label 固定是 `dev.ccnm.controller`，换个位置再装一次就把原来那个换掉。要在同一个账号上和日用的并存跑另一份，只能手工另写一个 Label 的 plist，收尾也得手工——`ccnm controller uninstall` 只认固定的那个。

### 会话状态是 unknown

**unknown 是终态，不会自己变好。** 它表示 ccnm 证明不了这个会话的下落，不表示失败。

正确做法是去现场看：工作树、`git status`、Runtime 上的进程列表。**不要重试**——那个 Agent 可能已经改了文件、跑了命令。"不确定有没有执行"和"确定没执行"是完全不同的两件事，只有后者重发才安全。

### machine API 那边

`ccnm rpc` 挂掉不会停掉已经接受的会话——它们属于磁盘上的记录，不属于那条连接。重新连上来用 session id 照样查。

服务端死于运行途中会留下 owner 已经不在的记录，之后读出来是 `unknown` 而不是 `failed`，理由同上。丢了 session id 只能靠 `start_key` 找回，所以凡是结果有意义的执行都该给一个键。详见[协议说明](protocol/README.md)。

## 部署与登录相关的动作要单独授权

创建系统账号、改 ACL 或防火墙、配置独立登录、替换正在运行的二进制或 controller——这些每一次都要单独获得明确批准。**规划过不等于授权执行。**
