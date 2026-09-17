# MCP 路径的 `exec_command` 加 OS 沙箱（P33，2026-09-17）

验收见 ROADMAP 的 P33，起因是 toexec V2-P1 的结论：exec-server 那一跳没有增量，但 `codex sandbox` 不经 RPC 就能给命令加上 Codex 自己那层 OS 沙箱。用法见[配置说明](../configuration.md#exec_sandbox)。脚本和每轮原始结果在 toexec 仓库的 [`evidence/v2-p/p33-sandbox/`](https://github.com/xwfe/toexec/blob/main/evidence/v2-p/p33-sandbox/)。

**一句话结论**：workspace 写 `exec_sandbox = "codex"`，这个 workspace 的每条 `exec_command`——Claude、Codex、外部 MCP 客户端三种入口都一样——就包进 `codex sandbox` 跑，权限对象是 Codex 0.154.0 发给它自己命令的那份，一字不改。实测 warm cache 的 `cargo build/test/run`、node、python、git 只读操作都正常，编译耗时没有差别，每条命令多约 40 ms；被挡的是工作区外写、HOME 写、`.git` 写（所以 `git commit` 失败）、网络（依赖下不了，放开网络也不行，因为 `~/.cargo` 在 HOME 下）。用户定的三件事：opt-in、默认不变；被挡就报失败，不给"不带沙箱重试"的路；Linux 在本机容器里测。全程零模型额度。

## 1. 实测：沙箱对日常项目操作做了什么（P33.1）

同一条命令跑两遍：D 直接跑（今天 `exec_command` 的样子），S 包进 `codex sandbox --sandbox-state-json <state> -- argv`，state 里的权限对象取自 P21 录下的 `process-start-workspace-write.json`。HOME 是一个新目录，`.cargo`、`.rustup` 是指向本账号真实目录的 symlink——P12 之后 Runtime 执行账号的样子（工具链装在自己 home 里）；写穿 symlink 会落到真实目录，正好验沙箱挡不挡。项目是一个依赖 `serde` 的小 crate，锁文件和注册表缓存在沙箱外先准备好；D 和 S 各用自己的 `target` 目录，S 真的从头编译。

### 1.1 macOS 26.6.2 arm64，Codex 0.154.0（Homebrew），cargo 1.98.0

| 操作 | 直接跑 | 沙箱里 |
| --- | --- | --- |
| `cargo --version`、`cargo build`（warm cache）、`cargo test`、`cargo run` | 正常 | **正常**，从头编译三次各 2.3–2.45 s，直接跑 2.2–3.0 s |
| `cargo build`，`CARGO_HOME` 指向不存在的目录（要下载） | 正常 | **失败**：`Couldn't resolve host` —— 没网络 |
| `git status`、`git stash list` | 正常 | 正常 |
| `git commit` | 正常 | **失败**：`Unable to create '.git/index.lock': Operation not permitted` |
| `node app.js`、`python3 -c` | 正常 | 正常 |
| 写 `target/`、写 `$TMPDIR`、写 `/tmp`、在子目录里跑 | 正常 | 正常 |
| 写工作区外、写 HOME、写 `.git/`、写穿 `~/.cargo` symlink | 写成 | **挡住**：`Operation not permitted`，文件不存在 |
| 读工作区外、读 HOME | 读到 | 读到（沙箱不挡读） |
| 连本机端口 | 连上 | **挡住**：`PermissionError: [Errno 1] Operation not permitted` |
| `ps` | 正常 | **挡住**：`sh: /bin/ps: Operation not permitted`，rc 126 |
| `/usr/bin/true` 往返，20 次 p50 | 3.1 ms | 39.7 ms |

**两个变体**（只为决定用，产品没提供）：`.git` 那条改成可写，`git commit` 成功、工作区外仍挡；`network` 改成 `enabled`，连端口成功，但 cold `cargo build` 仍失败——`~/.cargo-cold/registry` 在 HOME 下写不了。所以"能下载依赖"要网络和可写的缓存目录两样都有，光放开网络没用。

**包装器本身怎么失败**：程序不存在，`sandbox-exec: execvp() of '…' failed`，退出码 71；`--sandbox-state-json` 不是 JSON，退出码 1，stderr `Error: invalid --sandbox-state-json value`；权限对象是空的，退出码 134 且没有任何 stderr（abort）。命令自己的退出码原样透传（`exit 7` → 7）；命令被信号杀死时包装器报 128+n（TERM → 143），而不是自己死于信号。

**进程组**：从外面看，`codex sandbox` 起的 `sleep` 与包装器在同一个进程组——ccnm 以 `process_group(0)` 起包装器，超时时杀整组，杀得到里面的命令。

**环境**：沙箱给命令加了 `CODEX_SANDBOX=seatbelt`、`CODEX_SANDBOX_NETWORK_DISABLED=1`、`__CF_USER_TEXT_ENCODING`，并在 `PATH` 前面插了 `$CODEX_HOME/tmp/arg0/codex-arg0XXXX` 和 Homebrew 的 `codex-path`——**`codex sandbox` 会往 `CODEX_HOME` 里写**，所以 ccnm 给它的是自己造的私有目录，不是执行账号的 `~/.codex`。

### 1.2 Linux（本机 OrbStack 容器，Debian bookworm aarch64，Codex 0.154.0 官方 musl 发行包，bubblewrap 0.8.0）

同一脚本、同一权限对象，容器按 P21 的做法起（`--security-opt seccomp=unconfined`，否则 bwrap 建不了 user namespace），执行账号 `runner`，Rust 1.98.1 经 rsproxy 镜像装在它 home 里。**结论与 macOS 一致**，差别只在报法和进程结构：

| 项 | macOS（Seatbelt） | Linux（bubblewrap） |
| --- | --- | --- |
| 被挡的写 | `Operation not permitted` | **`Read-only file system`**（rc 2；`git commit` 是 `Unable to create '.git/index.lock': Read-only file system`） |
| 被挡的网络 | `connect` 时 `Operation not permitted` | **建 socket 时**就 `Operation not permitted` |
| `ps` | `Operation not permitted`，rc 126 | `fatal library error, lookup self`，rc 1（pid namespace 里没有 `/proc`） |
| 程序不存在 | `sandbox-exec: execvp() … failed`，rc 71 | `codex-linux-sandbox` **panic**：`Failed to execvp …`，rc 101 |
| 空权限对象 | rc 134，无输出 | rc 1，`bwrap: execvp codex-linux-sandbox: No such file or directory` |
| 退出码 / 信号透传 | 7 / 143 | 7 / 143 |
| `/usr/bin/true` 往返 p50 | 3.1 → 39.7 ms | 0.3 → 14.4 ms |
| 从头编译三次 | 2.3–2.45 s，直接 2.2–3.0 s | 1.31–1.42 s，直接 1.31–1.32 s |
| 进程组 | 命令与包装器同组 | **不同组**：`codex sandbox` → `codex-linux-sandbox`（同组）→ `bwrap`（自己一组）→ 沙箱内的 `codex-linux-sandbox`/`sh`/`sleep`（另一个 session）。P24 已知 Codex 的 bwrap 带 `--unshare-pid --die-with-parent`：杀掉包装器那一组，bwrap 随父进程死，它的子进程是 pid namespace 的 init，整个 namespace 一起没了。ccnm 的超时杀组靠的是这条链，不是组号；用真实 ccnm 二进制在容器里验过（第 3 节的超时子项） |

两个变体（`.git` 可写、网络放开）和 macOS 结果相同：放开网络后 cold `cargo build` 仍因 `~/.cargo-cold` 是 `Read-only file system` 失败。沙箱加的环境变量只有 `CODEX_SANDBOX_NETWORK_DISABLED=1` 和 `PATH` 前面的 `$CODEX_HOME/tmp/arg0/…`（没有 `CODEX_SANDBOX=seatbelt`）。没装 node，L9 跳过。

**容器里用真实 ccnm 二进制跑集成测试时撞到的一条**：测试夹具把 ccnm 的状态目录放在 `/tmp` 下，Codex 拒绝在临时目录里建它的沙箱辅助程序（`WARNING: proceeding, even though we could not create PATH aliases: Refusing to create helper binaries under temporary dir "/tmp"`），随后每条命令都死在 `bwrap: execvp codex-linux-sandbox: No such file or directory`，退出码 1——**而这被当成模型的命令失败报了出来**，正是 P33.2 说不能发生的事（脚本实测没撞上，因为它的 `CODEX_HOME` 在证据目录下）。macOS 上同样的夹具不受影响（Seatbelt 不需要这个辅助程序）。这条催生了第 2 节的启动探针。

## 2. 做了什么（P33.2、P33.3）

- **配置**：workspace 新字段 `exec_sandbox`，值 `off`（默认）/ `codex`。Runtime 自己的配置，不上 wire——MCP server 启动时本来就读 Runtime 配置（`ExecGate::decide`），沙箱在同一处解析。
- **`crates/ccnm-core/src/mcp/sandbox.rs`（新）**：`Sandbox::resolve` 在 server 启动时定下来——没 `codex_bin`、版本不是 0.154.0（和 exec-server 链共用 `provider::codex::check_measured`）、找不到状态目录，会话启动就拒（`CCNM_E_CONFIG` / `CCNM_E_VERSION`），不退回裸跑。`Sandbox::wrap` 把 `Cmd` 的程序和参数挪到 `codex sandbox --sandbox-state-json <state> --` 后面，cwd、环境、超时不动，另设 `CODEX_HOME` 指向 ccnm 在状态目录下建的私有目录（随 server 结束删除）。权限对象由 `permission_profile()` 生成，测试 `the_profile_is_the_one_codex_sends_for_its_own_commands` 把它和 P21 的 fixture 逐字段比对；`file://` URI 按 URL 规则百分号编码。
- **启动探针**：`Sandbox::resolve` 在版本核对之后用沙箱跑一条 `sh -c 'exit 0'`，退出码非 0 就拒绝会话（`CCNM_E_DEPENDENCY: the exec_command sandbox does not work on this Runtime`，带 Codex 自己的 stderr）。它证明包装器起得来，不证明它在管束——管束靠第 1 节的实测和真 Codex 那条测试。能抓到的：Linux 没装 bubblewrap、建不了 user namespace、状态目录在 `/tmp` 下（第 1.2 节）。代价：会话启动多一次往返，15–40 ms。`codex --version` 失败时现在也报退出码和 stderr（原来只说 failed，排错时分不清是二进制不在还是解释器不在）。
- **`exec_command`**：开了沙箱时先按 `execvp` 的规则找程序（`sandbox::locate`），找不到仍报 `CCNM_E_DEPENDENCY`——否则沙箱启动器的退出码 71（Linux 是 101）会冒充命令结果；包装器本身起不来报的是 `codex_bin` 不可运行，不是命令名。每条结果的 `notes` 和正文末尾多一行 `[sandboxed: …]`。
- **不做的**：不区分"沙箱挡的"和"命令自己失败的"——两者都是退出码非 0 加 `Operation not permitted`（Linux 上是 `Read-only file system`），Codex 自己也只能靠猜（`is_likely_sandbox_denied` 看退出码和文本）。不给模型"不带沙箱重试"。不改 doctor：开关在会话启动时就把问题报出来。

## 3. 测试

**单元**（`mcp::sandbox`，5 个）：权限对象等于 fixture；state 里 `workspaceRoots` 是根、`sandboxCwd` 是命令目录；带空格和中文的路径百分号编码；`wrap` 之后程序变成 `codex_bin`、原程序和参数在 `--` 之后、cwd/超时/环境保留、`CODEX_HOME` 在状态目录下且随 `Sandbox` 删除；`locate` 按 `execvp` 规则找程序（PATH、`./` 相对 cwd、目录不算）。

**集成**（`crates/ccnm-cli/tests/exec_sandbox.rs`，经真实二进制的 `internal mcp-serve`，执行端是 `tests/fixtures/fake_codex_sandbox.py`——不做任何沙箱，只记下 ccnm 给它的 state、argv、cwd、`CODEX_HOME`，再带着 `FAKE_SANDBOXED=1` 运行命令）：

| 测试 | 证明了什么 |
| --- | --- |
| `a_workspace_without_the_switch_runs_commands_bare` | 同一节点配了 Codex，没开开关的 workspace 一条命令都不经过包装器 |
| `the_switch_wraps_every_command_with_the_measured_profile` | 会话开头恰好一次探针（`/bin/sh -c 'exit 0'`），然后三条命令全部经过包装器；每条的权限对象等于 fixture、`workspaceRoots` 是根、`sandboxCwd` 跟着 `cwd` 参数走；结果正文第一行是命令本身而不是包装器、末尾带沙箱说明；失败的命令仍是结果不是错误；`CODEX_HOME` 在 ccnm 状态目录的 `exec-sandbox/` 下、server 结束后已删 |
| `a_missing_program_is_still_a_dependency_error_not_a_result` | `/nonexistent/program` 和 `./no-such.sh` 都报 `CCNM_E_DEPENDENCY`，没有到达包装器 |
| `a_runtime_that_cannot_provide_the_sandbox_refuses_the_session` | 没 `codex_bin` 报 `CCNM_E_CONFIG` 并点名 `exec_sandbox`（同一 Runtime 上没开开关的 workspace 照常）；版本 0.155.0 报 `CCNM_E_VERSION`；假 Codex 的 `FAKE_SANDBOX_FAIL` 让每次 `sandbox` 调用退出 1 并打一句 bwrap 的话，会话报 `CCNM_E_DEPENDENCY` 并原样带上那句，没有任何命令跑过 |
| `against_the_real_codex_sandbox_when_configured` | 设 `CCNM_TEST_CODEX_BIN` 才跑：真 Codex 0.154.0 下工作区内写成功、工作区外写按失败报且带 `Operation not permitted` / `Read-only file system`、连本机端口被挡；一条 `sleep 300` 在 1 秒超时后报 `timed out`，10 秒内进程表里没有它——macOS 上它在包装器的进程组里，Linux 上靠 bwrap 的 die-with-parent 链。本机 macOS 1 passed；Linux 见第 4 节 |

## 4. 门禁

macOS 26.6.2 arm64，rustc 1.98.0：

- `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：807 passed / 0 failed（合并 P31 后 797，本阶段新增 5 个单元 + 5 个集成）
- `CCNM_TEST_CODEX_BIN=/opt/homebrew/bin/codex cargo test -p ccnm-cli --test exec_sandbox against_the_real`：1 passed（含探针、被挡的写和网络、1 秒超时后沙箱内的 `sleep` 没活下来）；同样带真 Codex 的 `exec_serve against_the_real`：1 passed
- **Linux 容器**（OrbStack Debian bookworm aarch64，用户 `runner`，rustc 1.98.1，Codex 0.154.0 musl 发行包，bubblewrap 0.8.0）：把工作树拷进去，`CCNM_TEST_CODEX_BIN=/usr/local/bin/codex cargo test -p ccnm-cli --test exec_sandbox`：**5 passed / 0 failed**，其中真 Codex 那条的超时子项在日志里可见 ccnm 的看门狗杀掉包装器后沙箱里的命令也没了。修探针之前同一组是 4 passed / 1 failed（第 1.2 节那条）
- `cargo +1.89 check --workspace --all-targets --locked`：通过、0 条警告
- `python3 scripts/check_protocol.py`、`python3 -B -m unittest discover -s tests -p 'test_*.py'`、`python3 scripts/check_plan.py`、`git diff --check`：通过

## 5. 没做到、没测到的

- **`codex sandbox` 的参数和权限对象形状同样按 0.154.0 实测**，受同一个版本 pin 约束；比封存的原生链省下的是协议、规则表、监督进程和 fs helper 那一整层，不是版本核对。
- 没有真机、没有真实模型回合；Linux 只在本机 aarch64 容器里验过（脚本和 ccnm 的集成测试都跑了，见第 4 节），CI 的 Linux job 没有 Codex，跑不到真 Codex 那条。
- 没测：被沙箱挡住时模型会怎么反应（会不会反复试）；带 `cwd` 参数指向 symlink 目录时 `sandboxCwd` 和沙箱判定是否一致；`$TMPDIR` 没设时 `tmpdir` 条目解析到哪。
- 探针只证明包装器起得来。一台机器上沙箱"起得来但不管束"（比如 Seatbelt 被系统策略放宽）探针看不出，也没有便宜的办法在每次会话启动时证明管束。
- 两个变体（`.git` 可写、网络放开）只量了，没提供开关；要提供得先按第 1 节的表重新量一遍。
