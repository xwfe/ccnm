# P7.3 真机：Claude 的对照与 dogfood 都过了，Codex 没跑成（2026-09-10）

P7.3 要在同一套环境里做三件事。**做完两件**：并入的原 P6.3 内容（用公共 API 跑真实 provider 的双机闭环、与人类 CLI 对照）**判定 pass**；真实项目的完整 dogfood（启动→修改→测试→结果→停止→恢复）**也过了**。没做的是 Codex 那一半，原因见第七节。

两件事隔了一个小时，用的是同一套环境、同一个构建、同一个 Runtime 执行身份。

## 一、环境

| 角色 | 机器 | 身份 | 说明 |
| --- | --- | --- | --- |
| Agent Node | fodelf（Mac mini） | fodelf | 官方 Claude Code 2.1.267，凭据只在这台 |
| Runtime Node | 本机（xdwmbp） | ccrun（uid 504） | 项目在这里，MCP 以隔离身份执行 |

两个 workspace：对照用 `p7parity`（`/Users/Shared/ccnm-p7-20260910/parity`，空 git 仓库），dogfood 用 `p7dogfood`（`/Users/ccrun/p7-dogfood`，ccnm 自己的一份真克隆，起点 26d27de）。

P3 用了两个相反方向，这一轮只用一个：两个 provider 的 CLI 不在同一台，Claude 在 fodelf，所以方向由登录位置决定，不是设计选择。

本机三步特权准备（装公钥、SSH 准入、换专用主组）由用户执行，脚本在 `scripts/p7-*.sh`，root 清单 `/var/db/ccnm-p7-local-20260910`。**没有创建任何新账号**——ccrun 是既有账号，只改了主组和准入。

## 二、隔离先立住了

新开一条 SSH 连接、以 ccrun 身份探，逐项区分"拒绝"和"不存在"（P3 吃过 `[ -e ]` 把权限拒绝误报成 absent 的亏）：

```text
uid=504(ccrun) gid=504(ccrun)      staff(20) 与 admin(80) 都不在
/Users/bing 及其下 .codex/.claude/.ssh/auth.json   全部拒绝
/var/run/docker.sock               拒绝
sudo                               需要密码
```

`ccnm doctor p7parity` 以 ccrun 身份跑，**0 failed**，两个 SKIP 是文档写明只有活会话或 ccnm 之外才能证明的：

```text
Runtime user OK ccrun / No sudo OK / Not an admin OK / No SSH keys OK
No Claude credential OK / Runtime safety OK ×2 / No Docker socket OK
exec_command OK the runtime account is confined
Agent selection OK agent/claude-main (Claude Code; profile default)
Controller OK ccnm 0.2.0, pid 53233, Aqua
Claude Code OK 2.1.267 / Claude authentication OK via claude.ai (max)
Reverse SSH OK / Remote MCP handshake OK initialize 494 ms, 7 tools
```

**跑完真活之后又探了一遍**（生产边界复核，dogfood 两个会话结束后、新开连接）：结果一模一样——ccrun 不在 staff/admin/wheel，`/Users/bing` 下的 `.claude`/`.codex`/`.ssh`/`auth.json` 全部 `Permission denied`，`sudo -n` 报需要密码，`SSH_AUTH_SOCK` 未设置，环境里没有任何 `ANTHROPIC_*`/`CLAUDE_*`/`CODEX_*`/token/key 形状的变量，`security find-generic-password -s "Claude Code-credentials"` 找不到条目。**Agent 在这台机器上跑了 68 个 turn 的真实任务，没有把边界撑开一寸。**

`/var/run/docker.sock` 那条要说准确：它**存在**，是一个指向 `/Users/bing/.orbstack/run/docker.sock` 的 symlink，ccrun 读不到目标——用不了是对的，但 doctor 那句 `no Docker socket on this machine` 说过头了（见第六节末尾）。

## 三、对照结果：pass

证据 [tests/fixtures/p7-parity/claude.json](../../tests/fixtures/p7-parity/claude.json)，工具是 `scripts/p7_parity_check.py`。

**比副作用，不比模型说的话。** 两条腿各被要求在工作树里写一个带一次性 token 的文件，脚本回读存在性、内容和属主。七项全过：

| 检查 | 结果 |
| --- | --- |
| 人类 CLI 成功 | 退出码 0；`claude exited 0 in 31.5 s`，8 turns，$0.14 |
| 人类 CLI 副作用 | 产物内容正确，属主 uid=504 |
| machine API 成功 | `completed`，exit_code 0 |
| machine API 副作用 | 产物内容正确，属主 uid=504 |
| **两条腿同属主** | 都是 504 |
| machine API 无私有数据 | home 路径、`.claude`/`.codex`、`auth.json`、私钥名、`session_dir`、`controller` 一个都没出现 |
| provider 自报身份 | 声明 claude，Agent 报 claude |

**同属主那条是这次最值钱的一行。** 它说明两个公开入口最终落到同一条隔离执行链上，machine API 没有悄悄换个身份执行。

顺带验掉一件本轮刚修的事：`usage {"input_tokens": 10, "output_tokens": 1031}`、`cost {"total_usd": 0.0856755}` 真的出现在 `session.result` 里。那两个字段之前被生产转换写死成 `None`，协议承诺了却从不发送；这是修完之后第一次真机确认。

## 四、对照那一轮暴露的四个问题

### 1. `ccnm controller install` 不替换已在监听的 controller

`scripts/deploy.sh` 打印 `listening: ccnm 0.2.0 as fodelf, pid 1716, Aqua`，读起来像重启成功。实际 pid 1716 是 **9 月 5 日**起的进程，跑的是五天前的二进制，而二进制当天 15:50 才换。

`ccnm doctor` 也没抓到：它显示的 `0.2.0` 是版本字符串，同版本号的新旧构建一模一样。

症状出现在毫不相干的两行上：

```text
Claude Code             FAIL   CCNM_E_VERSION: remote ccnm speaks protocol 3, this one speaks 1
Claude authentication   FAIL   CCNM_E_VERSION: remote ccnm speaks protocol 3, this one speaks 1
```

`uninstall` 再 `install` 之后是 pid 53233，两行立刻变 OK。另外 `uninstall` 移除了 plist 和 socket，但**旧进程没退**——它的子命令名是 `internal work-controller`（更老的构建），当前 launchd 标签管不到它。排查办法写进了[运维手册](../operations.md)。

### 2. `No SSH keys` 是位置启发式，不是隔离

架构要求 Runtime 拨号去 Agent（`launcher::run_print_with_agent` 里那条 ssh），所以 Runtime 身份必须持有一把可用私钥。而 `No SSH keys` 检查只扫 `~/.ssh`：

- 私钥放 `~/.ssh/` → FAIL
- 同一把私钥挪到 `~/.config/ccnm/transport/` → **OK，而且照用不误**

这条检查自己的注释写着"Runtime 能读到的私钥就是 Runtime 能用的私钥，而 `exec_command` 是个 shell"。挪个位置并不改变这一点，只是让检查看不见。

于是 [production-safety.md](../production-safety.md) 的"最终门禁"——那七行全 OK——在 mcp-ssh backend 下**只有把传输密钥放到被扫目录之外才达得到**。这不是绕过，是当前设计下唯一的达标路径，但文档没说，而 `No SSH keys` OK 声称的东西比它证明的多。**要么文档写明传输密钥该放哪、并把这条检查的含义降级成"标准位置没有多余私钥"，要么换一种真能隔离的机制。** 本轮只记录，不改实现。

### 3. `~/.claude` 是 0755 就选不上 instance

全新装的 Claude 配置目录是 0755，而 ccnm 要求私有。报错出现在 Runtime 侧，且不提权限二字：

```text
Agent selection   FAIL   CCNM_E_VERSION: Agent probe identity differs from the Runtime selection
```

真正的原因要手工跑一次 `ccnm internal probe` 才看得到：

```text
CCNM_E_AUTH: dedicated Agent home must be private, owned by the execution
identity and free of symlinks
```

`chmod 700 ~/.claude` 就好。两处措辞都值得改：Runtime 侧那条应该把 Agent 的原因带回来，而不是只说"身份不一致"。

### 4. `session.start` 要求 `agent.node`，而调用方不许选 node

对照工具只给了 `--instance claude-main`，得到 `-32602: node is required and must be a non-empty string`。

协议规定 node 必须是配置里绑定的那个，客户端不能换机器——既然如此，让调用方把它抄回来就是纯粹的摩擦：知道 instance 的客户端还得先查一次 `agents.list`。

工具已经改成自查（同名 instance 匹配到 0 个或多个就报错，不猜）。**协议要不要放宽成"给了 instance 可以省 node"是 v1 定稿前该决定的事**，本轮不改。

代价是实打实的：第一轮真机在人类 CLI 那条腿已经花掉额度之后才栽在这儿。

## 五、真实项目 dogfood：通过

前一轮的工作树是个空 git 仓库，证明不了"一个真实任务能不能交付"。这一轮换成真项目：把 ccnm 自己 clone 一份到 Runtime 执行身份自己的家目录（`/Users/ccrun/p7-dogfood`，起点 26d27de），注册成 workspace `p7dogfood`，`ccnm doctor p7dogfood` 以 ccrun 身份跑 **0 failed / 2 not checked**。

选 ccnm 自己是因为它满足两个硬条件：工具链在被隔离的身份手上够用（`/usr/bin/python3` 3.9.6、`/usr/bin/git`、clang、make；**没有 cargo、没有 node**），而且有一套真的、会红会绿的测试。Rust 那半在这个身份下根本跑不了，这本身就是结论的一部分。

### 两条腿，两个会话，同一个工作树

| | 会话 A | 会话 B |
| --- | --- | --- |
| session id | `a9cc7d3c-…05b4` | `8c36fad8-…5fa7da`（**新的**） |
| 任务 | 把 `check_plan.py` 的链接检查从 5 个入口文档扩到全仓库 Markdown，补测试 | 修上一轮发现的 Python 3.9 导入错误 |
| 结果 | exited 0，262.7 s，38 turns，$1.43，0 permission denials | exited 0，175.3 s，30 turns，$0.61，0 permission denials |
| 产出 commit | `7b22edb` | `3b49d9a` |

**恢复那条判据落在会话 B 上**：A 结束、guard 释放、进程归零之后，B 是一个全新的 session id，开场先 `git log` 看见了 A 的提交，接着在同一个工作树上往下做。工作树状态跨会话留存，不靠会话本身留存。

`ccnm result p7dogfood` 能把 B 的完整结果重新取回来（`(print)` 标记、退出码、turns、usage、cost、正文），也就是说终端断了结果不丢。

### 交付物是真的，已经合进 main

两个 commit 原样 fast-forward 进了本仓库 main，**hash 没变**——`7b22edb` 和 `3b49d9a` 就是那两个会话产出的对象本身。内容：

- `7b22edb`：`check_links()` 以前只看 5 个入口文档，`docs/` 下 34 个 .md 的相对链接从没被检查过。改成走全仓库，`.git/` 和 `target/` 在任意层级剪掉；补 3 条测试（坏链接放在原来查不到的 `docs/research/` 下、构建目录被忽略、协议头与锚点不当文件查而 `../../` 越界要报错）。现有 39 个 .md、101 条相对链接一条没坏，所以没改任何文档链接。
- `3b49d9a`：`tests/test_blackbox_client.py` 少一行 `from __future__ import annotations`，在 Python 3.9 上 import 就 `TypeError: unsupported operand type(s) for |`，20 个黑盒契约用例连 discover 都进不去。

两条都做了红绿对照：新测试在改动前的实现上跑是 2 failures + 1 error；修 3.9 之前是 1 error / 之后 `Ran 120 tests, OK (skipped=22)`，22 个 skip 是缺 `target/{debug,release}/ccnm` 导致的预期跳过，不是掩盖。

**验收不采信模型的自述。** 上面每条都由我在 ccrun 身份下另跑一遍确认：`git log`、`git status --short`（干净）、`python3 -m unittest discover`（120 passed / 22 skipped）、`python3 scripts/check_plan.py`、`git diff --check`。合进 main 之后在本机又跑了 3.12 和 3.9 两个解释器，各 120 passed。

### 停止与资源归零

`ccnm stop p7dogfood` → 退出码 3，`CCNM_E_NOT_READY: no verifiable selected session is running`（print 会话结束时已经自己收干净了，见第六节第 6 条）。停止后逐项复核：`ccnm status p7dogfood` 无活会话、两个写入 guard 都是 `released`、ccrun 名下 `mcp-serve` 进程 0 个。

## 六、dogfood 暴露的六个问题

都是真机才会撞上的，离线测试一个都碰不到。

### 1. 工作树属主和执行身份不一致，Agent 侧的 git 全废

第一次把克隆放在 `/Users/Shared/…/dogfood`（bing 所有、0777、ccrun 可写）。文件写得进去，但 ccrun 跑任何 git 命令都是：

```text
fatal: detected dubious ownership in repository at '/Users/Shared/ccnm-p7-20260910/dogfood'
```

而 `ccnm doctor` 那一行是绿的：`Workspace root OK … is a directory for ccrun`——它查的是可写，不是属主。**可写不等于可用**：现代 git 对属主不是自己的仓库直接拒绝工作。改成放在 ccrun 自己拥有的目录后一切正常。

要么 doctor 把属主一起查了并说清后果，要么文档写明"项目目录必须属于 Runtime 执行身份"。现在两样都没有。

### 2. `ccnm workspace add` 用操作者的身份校验路径

以 bing 注册 ccrun 的项目目录：

```text
CCNM_E_WRONG_WORKSPACE:
/Users/ccrun/p7-dogfood is not a directory on this machine
caused by: Permission denied (os error 13)
```

第一行读起来像"路径不存在"，真正的原因在第二行。以 ccrun 跑同一条命令就成功了。这条和下一条是同一个根：**ccnm 到底该由谁来跑，产品里没有一个一致的答案。**

### 3. 同一台机器上，doctor 的结论取决于谁在敲命令

同一个 workspace、同一个构建、同一时间：

| 谁跑 | 结果 |
| --- | --- |
| ccrun | **0 failed**，2 not checked |
| bing | **7 failed**：Runtime user / Not an admin / No SSH keys / No Claude credential / Runtime safety ×2 / exec_command |

因为身份类检查判的是**当前进程的身份**，而 `runtime_user = "ccrun"`。也就是说 ccnm 期望"操作者就是 Runtime 执行身份"。

但这和 [生产安全](../production-safety.md) 里写的冲突：那份文档说 `ccrun` 的 `~/.ssh` 里不许放私钥，可 `ccnm run` 要从 Runtime 拨号去 Agent Node，跑它的身份就必须持有一把可用私钥。两件事同时成立的唯一办法，是把传输私钥放到 `~/.config/ccnm/transport/`——检查只扫 `~/.ssh`，看不见就报 OK。

这就是上一轮记的 `No SSH keys` 那条findings的完整版本，现在两端的实测输出都有了。**这是 v1 定稿前必须给出的一个答案，不是措辞问题。**

### 4. `ccnm status` 看不见 `--print` 会话

会话跑着的时候：写入 guard 是 `held a9cc7d3c-… p7dogfood`、ccrun 名下有 `internal mcp-serve` 进程、Agent 侧 `internal agent-run` 也在，而

```text
$ ccnm status p7dogfood
tmux 3.7c on the Agent Node
no live sessions
```

`status` 只报 Agent Node 上的 tmux 会话，非交互的 print 运行不在其中。操作者据此判断"没人在用"，接着起第二个会话，撞上的就是被占的写入 guard——而那个失败长得像"machine API 坏了"。这条和协议里 `-32008` 那个"实现从不返回的错误码"是同一个窗口的两面。

### 5. 新建的执行身份没有 git 身份，commit 直接失败

```text
fatal: unable to auto-detect email address
```

Agent 自己绕过去了：查了历史提交的作者，用 `git -c user.name=… -c user.email=…` 只对那一条命令生效，没往 `.git/config` 或全局配置里写东西。做法是对的（不擅自改机器配置），但**机器上至今没有持久的 git 身份**，下一个会话还会撞同一堵墙。doctor 不查这个，运维手册也没写。

### 6. `ccnm stop` 对已经结束的会话不是幂等的

print 会话结束后再 stop，得到退出码 3 和 `CCNM_E_NOT_READY`。措辞是诚实的（确实没有可停的会话），但清理脚本照着运维手册无脑调一次 stop 就会拿到非零退出，看起来像清理失败。

顺带一条措辞不准：`No Docker socket` 报的是 `no Docker socket on this machine`，实际 `/var/run/docker.sock` 存在，是个指向 `/Users/bing/.orbstack/run/docker.sock` 的 symlink，ccrun 读不到目标所以判成"没有"。结论没错（确实用不了），话说过头了。

## 七、Codex 没跑：不是失败，是没条件

Codex 装在本机（xdwmbp），不在 fodelf，而且**当前没有额度**。

这意味着 Codex 方向需要相反的拓扑（本机当 Agent、fodelf 当 Runtime），要在 fodelf 上另建临时 Runtime 账号——一整套特权准备翻倍——而即使建好了也跑不动，因为没额度。

所以 P7.3 的"各跑一次真实 provider"**只完成了 Claude 一半**。已经验到的和没验到的要分清：

- Codex 的结果文档形状有 P0 的实测 fixture（0.153.4）兜底，不是没有依据；
- 但**经 machine API 跑一次 Codex 从未发生过**，`agent.provider` 报 codex、Codex 的 `usage` 形状、`cost` 永远缺席这三条在真机上都没验证过；
- 协议本身是 provider 无关的，Claude 这一轮把整条链路端到端走完了。

**用户已决定：等 xdwmbp 的 Codex 额度回来再补这一半，不拿"只有 Claude"的状态硬进 P7.4。** 所以 P7.3 不勾完成，P7.4 不开工。

准备工作里能先确认的已经确认：xdwmbp 上装的是 `codex-cli 0.153.4`，正好等于仓库 pin 的版本，额度回来时不需要重新测量 fixture（重新测量的五步流程见[支持矩阵](../support-matrix.md)）。要新做的只有相反拓扑那套特权准备：本机当 Agent、fodelf 当 Runtime，得在 fodelf 上另建一个临时执行身份。

## 八、本轮改动了什么（清理时要还原的）

| 对象 | 改了什么 | 怎么还原 |
| --- | --- | --- |
| 本机 ccrun | 主组 staff→ccrun(504)、加入 `com.apple.access_ssh`、authorized_keys 加一行 | `scripts/p7-*.sh --revert` / `--apply`，逆序 |
| 本机 | `/Users/Shared/ccnm-p7-20260910/`（对照工作树、bin、tools、results，外加两个 git bundle） | 整个删掉 |
| 本机 ccrun | `~/p7-dogfood`（ccnm 的克隆）、`~/p7-task-*.txt`、`~/p7-run-*.{sh,log}` | 随 ccrun 家目录清理；两个 commit 已合进 main，删掉不丢东西 |
| 本机 ccrun | `~/.config/ccnm/config.toml` 里多了 `[workspaces.p7dogfood]` | 随 ccrun 家目录清理 |
| 本机 bing | `~/.config/ccnm/config.toml` 换成新格式 | `config.toml.pre-p7-backup` |
| 本机 ccrun | `~/.config/ccnm/`、`~/.local/bin/ccnm`、`~/.ssh/{config,known_hosts}`、`~/.config/ccnm/transport/` 的密钥 | 随 ccrun 家目录清理 |
| fodelf | `~/.ssh/config` 加了带标记的 alias 区块 | 按标记整块删；备份 `config.pre-ccnm-p7-backup` |
| fodelf | `~/.ssh/authorized_keys` 加一行 ccrun 的公钥 | 删该行；备份 `authorized_keys.pre-ccnm-p7-backup` |
| fodelf | `~/.config/ccnm/config.toml`（原本不存在） | 删掉 |
| fodelf | `~/.claude` 权限 0755 → 0700 | `chmod 755` 可还原，但**没有理由还原**：这是收紧凭据目录 |
| fodelf | controller 换成当前构建（pid 53233） | 保留；旧的 pid 1716 是 9/5 的孤儿进程，socket 已不存在 |
| 订阅 | 对照三次 Claude print（约 $0.4）+ dogfood 两次（$1.43 + $0.61） | 不可撤销，本轮合计约 **$2.4** |

## 九、验证

```text
scripts/p7_parity_check.py --workspace p7parity --instance claude-main --provider claude
    → verdict pass，七项全过
ccnm doctor p7parity（以 ccrun）           → 0 failed，2 not checked
ccnm doctor p7dogfood（以 ccrun）          → 0 failed，2 not checked
dogfood 会话 A/B（以 ccrun）               → 都 exited 0，产出 7b22edb / 3b49d9a
以 ccrun 复核 dogfood 产物                 → 120 passed / 22 skipped，check_plan 通过，工作区干净
停止后复核                                 → 无活会话，两个 guard 均 released，mcp-serve 进程 0 个
```

合入 main 之后在本机重跑的全量离线门禁：

```text
cargo fmt --all --check                    → 通过
cargo clippy --workspace --all-targets -D warnings → 退出码 0（未接管道）
cargo test --workspace                     → 602 passed / 0 failed
python3 -m unittest discover -s tests      → 120 passed（3.12 与系统 3.9 各跑一遍）
python3 scripts/check_protocol.py          → 38 个 fixture 通过
python3 scripts/check_plan.py              → 通过
git diff --check                           → 通过
```

**没验证的**：Codex 经 machine API（从未发生过）、interactive 模式、Ctrl-D、egress/网络策略。dogfood 只覆盖了这个 Runtime 身份手上有的工具链——Python 3.9 与 git；**Rust 那半没跑过**，因为被隔离的身份 PATH 里没有 cargo，这不是"没做"，是当前隔离方案下做不到，要做得先决定给执行身份装什么工具链。
