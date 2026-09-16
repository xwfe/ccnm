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

## 七、没做的

- **没跑真机。** 上面那些复现都在本机用假 HOME 做的（`.claude/.credentials.json` 是空对象，`.ssh/id_ed25519` 是一行占位文本），没有连 fodelf、没有起官方 Agent、没有消耗订阅额度。原始现场记录在 gld 仓库 `docs/rfc/evidence/v2-h-read-chain.md`。
- **没有改 audit 本身。** 哪些 finding 算 Fail、哪些可豁免，一条没动；改的只有怎么把结论讲给人听。
- **没有按入口分开判。** 评估过按 `payload.entry` 让只读会话跳过凭据类 finding，结论是不做：见第四节。
- **`doctor` 的输出没改。** 它本来就该把所有行都列出来，那是"这台机器什么状态"，不是"这次为什么被拒"。
