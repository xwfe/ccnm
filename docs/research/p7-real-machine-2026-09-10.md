# P7.3 真机：Claude 双机闭环通过，Codex 没跑成（2026-09-10）

并入 P7.3 的原 P6.3 内容——用公共 API 跑真实 provider 的双机闭环、与人类 CLI 对照——**Claude 这一半做完了，判定 pass**。Codex 那一半没做，原因见第五节。

## 一、环境

| 角色 | 机器 | 身份 | 说明 |
| --- | --- | --- | --- |
| Agent Node | fodelf（Mac mini） | fodelf | 官方 Claude Code 2.1.267，凭据只在这台 |
| Runtime Node | 本机 | ccrun（uid 504） | 项目在这里，MCP 以隔离身份执行 |

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

## 四、真机才暴露的四个问题

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

## 五、Codex 没跑：不是失败，是没条件

Codex 装在本机（xdwmbp），不在 fodelf，而且**当前没有额度**。

这意味着 Codex 方向需要相反的拓扑（本机当 Agent、fodelf 当 Runtime），要在 fodelf 上另建临时 Runtime 账号——一整套特权准备翻倍——而即使建好了也跑不动，因为没额度。

所以 P7.3 的"各跑一次真实 provider"**只完成了 Claude 一半**。已经验到的和没验到的要分清：

- Codex 的结果文档形状有 P0 的实测 fixture（0.153.4）兜底，不是没有依据；
- 但**经 machine API 跑一次 Codex 从未发生过**，`agent.provider` 报 codex、Codex 的 `usage` 形状、`cost` 永远缺席这三条在真机上都没验证过；
- 协议本身是 provider 无关的，Claude 这一轮把整条链路端到端走完了。

用户要决定的是：拿这个状态进 P7.4 并带一条明确限制说明，还是等 Codex 额度回来补齐。

## 六、本轮改动了什么（清理时要还原的）

| 对象 | 改了什么 | 怎么还原 |
| --- | --- | --- |
| 本机 ccrun | 主组 staff→ccrun(504)、加入 `com.apple.access_ssh`、authorized_keys 加一行 | `scripts/p7-*.sh --revert` / `--apply`，逆序 |
| 本机 | `/Users/Shared/ccnm-p7-20260910/`（工作树、bin、tools、results） | 整个删掉 |
| 本机 bing | `~/.config/ccnm/config.toml` 换成新格式 | `config.toml.pre-p7-backup` |
| 本机 ccrun | `~/.config/ccnm/`、`~/.local/bin/ccnm`、`~/.ssh/{config,known_hosts}`、`~/.config/ccnm/transport/` 的密钥 | 随 ccrun 家目录清理 |
| fodelf | `~/.ssh/config` 加了带标记的 alias 区块 | 按标记整块删；备份 `config.pre-ccnm-p7-backup` |
| fodelf | `~/.ssh/authorized_keys` 加一行 ccrun 的公钥 | 删该行；备份 `authorized_keys.pre-ccnm-p7-backup` |
| fodelf | `~/.config/ccnm/config.toml`（原本不存在） | 删掉 |
| fodelf | `~/.claude` 权限 0755 → 0700 | `chmod 755` 可还原，但**没有理由还原**：这是收紧凭据目录 |
| fodelf | controller 换成当前构建（pid 53233） | 保留；旧的 pid 1716 是 9/5 的孤儿进程，socket 已不存在 |
| 订阅 | 三次 Claude print（两轮对照，第一轮的 machine API 腿没跑成） | 不可撤销，合计约 $0.4 |

## 七、验证

```text
scripts/p7_parity_check.py --workspace p7parity --instance claude-main --provider claude
    → verdict pass，七项全过
ccnm doctor p7parity（以 ccrun）           → 0 failed，2 not checked
python3 -m unittest tests.test_p7_parity   → 44 passed
```

**没验证的**：Codex 经 machine API、interactive 模式、Ctrl-D、egress/网络策略、真实项目的完整 dogfood（启动→修改→测试→结果→停止/恢复）。dogfood 需要工作树里有真项目和工具链，本轮的工作树只是个空 git 仓库。
