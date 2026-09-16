# 只读会话被拒时，那条消息在指错地方（P18，2026-09-16）

起因是在 gld 仓库验证远端只读链（gld RFC-0002 的 H2）时撞到的：给一个 workspace 配 `external_mcp = "read"`，从另一台机器跑

```bash
ccnm mcp bridge <ws> --node <node> --mode read
```

握手直接失败，stderr 上是：

```text
CCNM_E_POLICY:
the runtime is running as fodelf and is not confined, so exec_command is refused:
  - Not an admin: this account is in admin, which is a route to root
  - No SSH keys: a possible private SSH key is accessible or unknown
  - No Claude credential: the Runtime identity can access a known Agent credential file or container
Runtime initialization is also refused: ... set allow_unisolated_credentials = true ...
See docs/production-safety.md. To accept an unconfined runtime for one workspace anyway,
set allow_unconfined_exec = true on it in config.toml.
```

只读会话一共四个工具（`workspace_info` / `read_file` / `list_files` / `search_text`），**没有 `exec_command`**。

## 一、先查这道闸是不是故意的：是

`Server::new` 里 `agent_boundary_clear` 那道闸无条件执行，跟会话是不是只读无关。查下来是有意为之：

- `crates/ccnm-cli/tests/external_mcp.rs` 里有 `agent_credentials_stop_the_external_entry_too`，用的就是 **read 模式**的 fixture，断言握手根本不发生。注释写着 "The credential boundary is not a property of the managed entry."
- P11.4 的记录（`cross-entry-p11-2026-09-11.md` 第四节）把它列为跨入口回归项：「Runtime 进程里放一个像 Agent 凭据的环境变量，外部会话**也**起不来。这条边界不属于受管入口」。
- `docs/production-safety.md` 写明这一类失败「会在 MCP 初始化、Git 探测前拒绝」。

所以**闸不动**。

一个容易误读的线索要说清楚：`with_gate` 里同一个判断写成 `write_guard.is_some() && !agent_boundary_clear(...)`，看起来像"只读会话有意跳过"。查历史不是：这个条件是 `3903801`（P3 的 single-writer）加的，当时 `Server::new` 永远传 `Some(guard)`，条件恒真；P10 之后 `new` 才会传 `None`，但那时 `new` 自己已经先判过了。唯一传 `None` 的调用方是 crate 内的测试 fixture，它的 audit 没有任何 finding。本轮把它改回无条件——**对现有全部调用方都是 no-op**，只是不再留下那个误导。

## 二、真正的缺陷：消息说的不是它做的事

三条，都实测过（假 HOME 里放 `.claude/.credentials.json` 和 `.ssh/id_ed25519`，真跑 `internal mcp-serve`）：

**1. 抬头说 `exec_command is refused`。** `Audit::refusal` 的第一行是写死的，两个闸共用。初始化这道闸拒的是整个会话，而只读会话压根没有那个工具。

**2. 列出了与本次拒绝无关的行。** `refusal` 打印 `self.failures()`——**全部** Fail。但 `agent_boundary_clear` 只读 `waived_by` 不豁免的那些：

```rust
fn waived_by(&self, accepted: Accepted) -> bool {
    if self.non_waivable() { return false; }          // 身份未知 / 认证环境
    if self.is_agent_credential() { return accepted.unisolated_credentials; }
    true                                               // 其余一律豁免
}
```

也就是说 `Not an admin`、`No SSH keys`、`Runtime user` 这三行**从来没参与过这个判断**，`unconfined_exec` 在这里一个 finding 都不豁免。挡住这次握手的只有 `No Claude credential` 一条。

**3. 结尾无条件推荐 `allow_unconfined_exec`。** 这是最贵的一条：它把人推去签一个名字叫「允许不受限地执行命令」的开关，来开一条只读链。而且**它根本不管用**——实测只写这一条，照样被拒，而且消息还在叫你去开你已经开了的那个开关：

```text
$ # 配置里已经有 allow_unconfined_exec = true
CCNM_E_POLICY:
the runtime is running as bing and is not confined, so exec_command is refused:
  ...
See docs/production-safety.md. To accept an unconfined runtime for one workspace anyway,
set allow_unconfined_exec = true on it in config.toml.
```

**只写 `allow_unisolated_credentials = true` 就够了**，实测握手成功、四个只读工具都在：

```text
initialize OK; serverInfo= {'name': 'ccnm', 'version': '0.7.0'}
tools: ['list_files', 'read_file', 'search_text', 'workspace_info']
```

原来的文档（`troubleshooting.md`）写的是"把两个开关都写上"。对一个有 `exec_command` 的受管会话是对的，对只读链是多签了一个不需要的东西。

## 三、改了什么

`Audit::refusal(accepted)` 变成 `refusal(accepted, Refused)`，`Refused` 两个取值：

| | `Refused::Session` | `Refused::ExecCommand` |
| --- | --- | --- |
| 抬头 | `... does not hold the Agent boundary, so no session is served here:` | 原文不变 |
| 列哪些 finding | 只列 `!waived_by(accepted)` 的 | 全部（exec 要求 `confined()`，即一条 Fail 都没有） |
| 结尾 | 只指 `allow_unisolated_credentials`，并明说另一个开关开不了这道门 | 原文不变 |

`exec_command` 那条路径**一个字都没改**——`mcp_read_file.rs` 的 `exec_command_is_refused_until_the_runtime_is_confined` 和 `troubleshooting.md` 里引用的措辞都还成立。

改后实测：

```text
CCNM_E_POLICY:
the runtime is running as bing and does not hold the Agent boundary, so no session is served here:
  - No Claude credential: the Runtime identity can access a known Agent credential file or container (private paths withheld)
    fix: use a separate execution identity with OS-denied access; do not copy or remove the Agent's login to make an audit pass
This identity can reach a known Agent login. To accept that for one workspace -- every command
the model runs could then read it -- set allow_unisolated_credentials = true on it in config.toml.
allow_unconfined_exec is a different admission and does not open this gate.
See docs/production-safety.md.
```

## 四、只读会话为什么该继续过这道闸

把理由写进了 `docs/production-safety.md` 和 `Server::new` 的注释。三类结论里，没有一条是关于 `exec_command` 的：

- **执行身份未知**：说不出这是哪个账号，`read_file` 返回的是谁的文件、`external_mcp = "read"` 是谁签的，都无从谈起。
- **认证环境继承**：只读会话一样起子进程——启动探 git，`search_text` 跑 ripgrep，环境里的 `ANTHROPIC_*` 照样传下去。
- **能读到 Agent 登录**：这一条要诚实说——**模型确实够不到**，工具路径全部限制在 workspace 根内，穿出去的 symlink 被 `mcp/path.rs` 拒掉。留着它不是因为只读会话能利用它，而是因为这条隔离是**这台机器的性质**，不是"这次给了多少权限"的性质；P11 要证的就是第二个入口没把第一个入口的边界撑大。代价是一行开关，不是一个专用账号。

## 五、协议影响

`docs/protocol/remote-workspace-mcp-v1.md` 第 11.2 节写着：「要机器判断就看退出码和第一行的名字，**不要解析后面的措辞**」。本轮改的就是那一行之后的措辞，退出码（33）和 `CCNM_E_POLICY` 都没变，schema 和 fixture 一个字没动。

## 六、门禁

```text
cargo fmt --all --check                                通过
cargo clippy --workspace --all-targets -- -D warnings  通过
cargo test --workspace                                 722 passed / 0 failed（本轮净 +3）
cargo test -p ccnm-cli --test external_mcp             24 passed（本轮净 +2）
python3 -m unittest tests.test_remote_workspace_mcp -q 10 passed
python3 -m unittest discover -s tests -q               168 passed
python3 scripts/check_protocol.py                      通过（38 + 21）
python3 scripts/check_plan.py                          通过
git diff --check                                       通过
```

## 七、真机复验（2026-09-16 同日，用户授权传独立文件）

上面第二、三节是本机假 HOME 做的。同日在真机上又跑了一遍完整的只读链。

**现场**：Host 是这台 Mac（arm64 macOS），Runtime 是 fodelf（Mac mini，arm64 macOS 25.3.0），账号 `fodelf` 在 admin 组、有 SSH 私钥、能读 Claude 凭据——正是 audit 会红三行的那种身份。走的是既有的 gld 探针 workspace `gldprobe`（`external_mcp = "read"`，root 在 `~/gld-remote-probe`）。

**边界**：用户批准的是"传独立文件，验完删"。fodelf 已装的 `~/.local/bin/ccnm`（sha256 `f3422ceb…`，Sep 16 00:37）**一个字节没动**，`config.toml`、`gldprobe.toml`、`ccnm-gldprobe` 也都没动；本轮新建 9 个文件（1 个二进制 `ccnm-p18-bin`、6 个包装脚本、3 份配置，配置里两个是新名字所以是 9 不是 10），跑完全删，删后逐项只读复核：残留为空、已装二进制哈希与时间戳不变、探针目录文件时间戳都早于本轮、无残留 `mcp-serve` 进程。

### 7.1 旧构建逐字复现了原始现象

fodelf 现装的 0.7.0（c720154 之前），配置两个开关都不写：

```text
CCNM_E_POLICY:
the runtime is running as fodelf and is not confined, so exec_command is refused:
  - Not an admin: this account is in admin, which is a route to root
  - No SSH keys: a possible private SSH key is accessible or unknown (names and contents withheld)
  - No Claude credential: the Runtime identity can access a known Agent credential file or container
...
See docs/production-safety.md. To accept an unconfined runtime for one workspace anyway,
set allow_unconfined_exec = true on it in config.toml.
```

和报告里的一字不差，连账号名都一样。

### 7.2 真机证明只读链只要一个开关

**这一条用旧构建就能证**，因为本轮没改 `waived_by` 也没改 `agent_boundary_clear`——改的只有措辞。把探针配置里的 `allow_unconfined_exec` 去掉、只留 `allow_unisolated_credentials`，真机 bridge 照样跑通：

```text
initialize OK, serverInfo: {'name': 'ccnm', 'version': '0.7.0'}
tools: ['list_files', 'read_file', 'search_text', 'workspace_info']
workspace gldprobe (not a git repository, macos/aarch64); [server pid 1895, call 1]
exec_command → CCNM_E_POLICY: exec_command is not available: this workspace is open for external MCP in read mode
```

也就是说 `gldprobe.toml` 里那句注释「这两个 opt-in」和 `troubleshooting.md` 原来那句「两个开关都写上」，在真机上是多签了一个。

### 7.3 新构建的消息在真机上正确

传上去的 `ccnm-p18-bin`（sha256 `92f1c02e…`，两端一致）：

```text
CCNM_E_POLICY:
the runtime is running as fodelf and does not hold the Agent boundary, so no session is served here:
  - No Claude credential: the Runtime identity can access a known Agent credential file or container (private paths withheld)
    fix: use a separate execution identity with OS-denied access; ...
This identity can reach a known Agent login. ... set allow_unisolated_credentials = true on it in
config.toml. allow_unconfined_exec is a different admission and does not open this gate.
See docs/production-safety.md.
```

`Not an admin` / `No SSH keys` 不再出现，抬头不再说 exec。**已经写了 `allow_unconfined_exec` 的那份配置**得到的是同一条消息——不再叫人去开他已经开了的那个开关。

新构建 + 只写凭据开关，四个工具真机全部实跑：`workspace_info` 报 `macos/aarch64` 和远端 pid、`read_file README.md` 读到 fodelf 上的真实内容和版本号、`search_text` 命中两个文件、`list_files` 与 `ssh fodelf ls` 一致、越界 `../.config/ccnm/config.toml` 被拒且错误里不出现本机绝对路径。

### 7.4 真机才暴露的：这条链上退出码恒为 0

远端 `mcp-serve` 自己退 33（在 fodelf 上直接跑，`remote exit=33`），但 `ccnm mcp bridge` 退 0。

**不是 ccnm 的缺陷**。分层测下来，`ssh -T fodelf "exit 33"` 也返回 0：

```text
debug1: Remote protocol version 2.0, remote software version Tailscale
Authenticated to fodelf.taila864e6.ts.net ([100.79.121.33]:22) using "none".
```

答话的是 Tailscale SSH，按 tailnet 身份授权——`production-safety.md` 那节讲 egress 时记过同一个东西。它不把远端的 exit-status 透传回来，所以 bridge `exec` 成的那个 ssh 拿不到 33。ccnm 这边没有可改的地方：bridge 本来就是让自己*变成* ssh，退出码是 SSH 服务端发不发 exit-status 决定的。

**但它影响怎么读契约。** 协议第 11.2 节说「要机器判断就看退出码和第一行的名字」。在这条链路上退出码不可用，Host 能拿到的只有 stderr 那段文字——而 `troubleshooting.md` 里已经记着 Claude Code 会把子进程 stderr 丢掉。两个凑一起，Host 就真的什么都没有了。这反过来说明本轮改的那段措辞比契约设想的更要紧：它可能是唯一到得了人眼前的东西。

已把这条限制写进协议文档 11.2 和排错手册；**没有改 bridge 的行为**，那是 SSH 服务端的属性。

顺带又印证一次：`search_text` 的参数是 `query` 不是 `pattern`，按 fixture 写会被参数检查挡掉。

## 八、没做的

- **没起官方 Agent，没消耗订阅额度。** 真机跑的全是 MCP 工具调用，没有模型请求。
- **没替换 fodelf 已装的二进制**，所以那台机器上用户自己那条 Agent 链路仍然是 c720154 之前的行为；要让它带上这个修复得另外授权。
- **没有改 audit 本身。** 哪些 finding 算 Fail、哪些可豁免，一条没动；改的只有怎么把结论讲给人听。
- **没有按入口分开判。** 评估过按 `payload.entry` 让只读会话跳过凭据类 finding，结论是不做：见第四节。
- **`doctor` 的输出没改。** 它本来就该把所有行都列出来，那是"这台机器什么状态"，不是"这次为什么被拒"。
- **退出码那条只在 Tailscale SSH 上测过。** 没有在一台普通 OpenSSH 服务端上做对照（本机没开 sshd），所以"普通 sshd 会透传 33"是按 SSH 协议推的，不是本轮量出来的。
