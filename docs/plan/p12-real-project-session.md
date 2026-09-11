# P12 真实远端项目会话计划（Linux Runtime）

这一轮要做的是 P12：把 Remote Workspace MCP 放到**一个真的远端项目、一台真的 Linux 机器、一个真的专用执行身份**上用一遍，然后才决定支持矩阵里那一项还是不是 experimental。

**写下来不等于授权执行。** 里面有创建系统账号、系统级装包、部署二进制和消耗订阅额度四类动作，每一类都要用户明确同意；其中要 root 的那一条由用户自己跑或明确说可以由我跑。

## 一、这一轮要证明什么

| 验收 | 怎么算通过 |
| --- | --- |
| **P12.1** | `scripts/p12_dogfood_check.py` 退出码 0：一个真项目走完 read → search → patch → build/test → read_output，改动收回去之后 Runtime 上的 `git status --porcelain` 为空；断开重连看到同一棵树；一次性资源归零 |
| **P12.2** | 同一个脚本里的 identity 段：`id -un` 是专用身份、不在 sudo/admin/wheel/adm/docker/staff、`sudo -n` 被拒、docker socket 不可写、`~/.ssh` 里没有出站私钥、`SSH_AUTH_SOCK` 不存在、读不到别人的 home；**且 `cargo`/`node`/`npm` 在 `exec_command` 里真叫得动**。egress 不作保证，明写 |
| **P12.3** | 版本错配、workspace 没 opt-in、read 想升 coding、writer busy、远端消失、Host 崩——六条各有可追溯结果，且失败之后远端没有孤儿 `mcp-serve`、写锁状态明确 |
| **P12.4** | README/usage/support-matrix/operations 写清与 Managed 入口的区别和支持平台，全部门禁通过后冻结 v1.x 契约 |

**不证明的**：第三个 Provider、HTTP gateway、generic SSH 管理、多租户、egress 边界。

## 二、环境

| | 机器 | 角色 |
| --- | --- | --- |
| Host / 客户端 | 本机（macOS，`bing`） | 跑 `ccnm mcp bridge` 和 MCP 客户端；真实 Host 那一腿跑 Claude Code 2.1.267 |
| Runtime | `hpsrv`（Debian 13 trixie，x86_64，6 核 15 GiB） | 跑 `ccnm internal mcp-serve`，执行身份是本轮新建的 `ccrun` |

选 `hpsrv` 是为了补证据空白：P3 到 P11 全部是 macOS → macOS，而 Managed 路线声称支持的"项目在 Linux 开发机"一次都没验过。

准备阶段已经确认的事实（只读，没有改动任何东西）：

- `hpsrv` 上 **Tailscale SSH 是关的**（`RunSSH: false`），`PasswordAuthentication no`，root 登录靠 key。所以这台机器上的 transport credential 是真的 OpenSSH 身份，不像 P7 记过的那种"Tailscale 按 tailnet 身份放行、密钥无所谓"的情况；
- 没有 `sshd_config` 的 `AllowUsers`/`AllowGroups`，新账号有 key 就能进，不需要动 ACL（macOS 上要动 `com.apple.access_ssh`，这里不用）；
- `/home/bing` 是 0700，`/home` 是 0755；`docker.sock` 属 `root:docker`，`sudo` 组里只有 `bing`；
- 机器上**没有** cargo、node、gcc/make、ripgrep；有 git、python3、curl、jq、xz、tar；
- 出站可达 `static.rust-lang.org`（装工具链要用）。

## 三、需要用户同意的四件事

1. **建一个专用账号** `ccrun`（无密码、不在 sudo/docker/adm、home 0700），并把本轮一次性公钥写进它的 `authorized_keys`。脚本：`scripts/p12-provision-linux-runtime.sh --apply`（要 root）。
2. **系统级装四个包**：`gcc libc6-dev make ripgrep`。前三个是 Rust 链接期要的（rustc 调 `cc`），`ripgrep` 是 **ccnm 自己**要的——`search_text` 调 `rg`，没有它七工具就少一个。同一个脚本装，`--revert` 按清单精确 purge。
3. **在 `ccrun` 自己的 home 里装工具链**：rustup（stable + clippy + rustfmt）和官方 Node 二进制包。脚本：`scripts/p12-runtime-toolchain.sh --apply`，**不要 root**，以 `ccrun` 身份跑。不碰 `/usr`，不碰别的账号。
4. **消耗一点订阅额度**：真实 Host 那一腿要用 Claude Code 连一次（`claude -p`，预计两三次）。脚本那一腿不花额度，必须先过。

不含、也不在本轮申请：改 ACL/防火墙/Tailscale 策略、动 `bing` 或 `git` 账号的任何东西、push/tag/release。

## 四、顺序

每一步做完停下来核对，不通过就不往下走。

### 0. 预检（只读）

两端 `id`、`ccnm --version`、`ssh` 可达性；`scripts/p12-provision-linux-runtime.sh --check` 和 `scripts/p12-runtime-toolchain.sh --check`（两个都只读）确认当前没有本轮残留。

### 1. 系统准备

```bash
sudo /Users/bing/xdw/ccnm/scripts/p12-provision-linux-runtime.sh --check   # 只读，先看
sudo /Users/bing/xdw/ccnm/scripts/p12-provision-linux-runtime.sh --apply
```

在 `hpsrv` 上跑。做完只读复核：`id ccrun`、home 权限、`authorized_keys` 行数、清单目录在不在。

### 2. 工具链（无特权，以 ccrun 身份）

```bash
ssh -i ~/.ssh/ccnm-p12-20260911 ccrun@hpsrv 'bash -s -- --apply' < scripts/p12-runtime-toolchain.sh
```

**判据是从客户端问一次非交互 ssh**：

```bash
ssh ccrun@hpsrv 'command -v cargo node npm rg git'
```

这一条不是形式主义。`exec_command` 的命令跑在非交互 ssh 会话里，而 Debian 的 `~/.bashrc` 第 6 行就 `return`，rustup 默认又把 PATH 写在文件末尾和只有 login shell 才读的 `~/.profile` 里——照默认装完，ccnm 报的是 `cargo is not installed on the Runtime Node, or is not on its PATH`，看着像没装。脚本因此自己写那一块，插在那个 `return` 之前。

### 3. 把 ccnm 装到 Runtime 上

Runtime 侧的 ccnm 由**它自己在那台机器上编译**，不交叉编译：

1. 本机 `git bundle create` 当前 HEAD，scp 到 `ccrun` 的 home（客户端推过去，`ccrun` 不需要出站凭据）；
2. `ccrun` 侧 `git clone` 那个 bundle 成 `~/projects/ccnm`——这就是本轮的 dogfood 项目，一棵**真的 git 仓库**（`git status` 要能答话）；
3. `cargo build --release` → `install -m 755 target/release/ccnm ~/.local/bin/ccnm`；
4. 顺手在那台机器上跑一次 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。**这是整个项目第一次在 Linux 上跑门禁**，结果不管绿还是红都是本轮的证据；红了就是真发现了平台假设，按缺陷处理，不改结论。

本机的 ccnm 用当前 build（`cargo build --release` 后装到 `~/.local/bin/ccnm`），替换前记原文件哈希并留 `.pre-p12` 副本。

### 4. Runtime 权威配置

在 `ccrun` 的 `~/.config/ccnm/config.toml` 里加三个 workspace，root 全在 `ccrun` 自己 home 下：

```toml
[nodes.runtime]
runtime_user = "ccrun"

[workspaces.p12rust]        # 真项目，coding
root = "/home/ccrun/projects/ccnm"
external_mcp = "coding"

[workspaces.p12read]        # 只读，另一棵树
root = "/home/ccrun/projects/readonly"
external_mcp = "read"

[workspaces.p12off]         # 没 opt-in
root = "/home/ccrun/projects/closed"
agent_node = "agent"
```

三件准备阶段就查明白、别在真机上现学的事：

- **两个 workspace 不能指同一棵树。** `p12read` 必须是另一棵树，否则 coding 会被 `Runtime workspace roots overlap after canonicalization` 拒。"两个入口看同一棵树"是靠**同一个 workspace 要 read 模式**证的（客户端可以要得比配置少），不是靠第二个 workspace。
- **"没 opt-in"的 workspace 必须有 `agent_node`。** 没有 Agent 又关着 external_mcp 的 workspace 谁都用不了，配置校验直接报 `requires agent_node or agent`。
- **不开 `allow_unconfined_exec`。** `ccrun` 是专用受限身份，本来就该放行；开了等于把要验的东西绕过去。

### 5. 脚本跑 dogfood（不花额度，必须先过）

```bash
scripts/p12_dogfood_check.py \
    --workspace p12rust --read-only-workspace p12read --closed-workspace p12off \
    --node hpsrv --runtime-user ccrun --runtime-home /home/ccrun \
    --other-home /home/bing --ssh-alias hpsrv-ccrun \
    --full-test-cmd 'cargo test --workspace' \
    --out docs/research/p12-dogfood-<日期>.json
```

它自己是一个 provider-neutral MCP 客户端，没有模型参与。判据全在 Runtime 侧，细节见脚本头部；离线自测见 `tests/test_p12_dogfood.py`。

### 6. 真实 Claude Code（这一步花额度）

MCP 配置指向 bridge：

```json
{
  "mcpServers": {
    "ccnm-p12rust": {
      "command": "ccnm",
      "args": ["mcp", "bridge", "p12rust", "--node", "hpsrv", "--mode", "coding"]
    }
  }
}
```

要它做一件**真事**：在那个远端 Rust 项目里定位一处东西、改它、在远端跑测试、把结果读回来。记下 Claude 那边的原文，然后到 Runtime 上核对文件内容和属主。P11 已经证过"真实 Host 能把它当 MCP server 用"，所以这一腿的重点不是握手，而是**真项目上的一次完整改动**是否成立。

**验收一律看 Runtime 侧副作用与属主，不采信模型的自述。**

### 7. 清理并复核

按逆序：一次性 workspace → 项目副本和 bundle → Runtime 上的 ccnm → 工具链（`p12-runtime-toolchain.sh --revert`）→ 账号和包（`p12-provision-linux-runtime.sh --revert`，要 root）→ 本机 `~/.ssh/config` 标记块和一次性密钥对。然后逐条只读复核归零，无法清理的如实写进记录。

**工具链和账号留不留由用户定。** 如果 `hpsrv` 以后要继续当 Linux Runtime，留着更省事；那就把"谁维护这套工具链"写进运维文档，而不是留一堆没人记得的东西。

## 五、资源清单

执行时把实际值填进这张表，清理时逐条核对：

| 资源 | 在哪 | 怎么撤 |
| --- | --- | --- |
| 一次性 SSH 密钥对 | 本机 `~/.ssh/ccnm-p12-20260911{,.pub}` | 删文件 |
| `ccrun` 的 `authorized_keys` 一行 | `hpsrv` | `p12-provision-linux-runtime.sh --revert` |
| `~/.ssh/config` 标记块（`hpsrv-ccrun`） | 本机 | 整块删除 |
| 账号 `ccrun` 及其 home | `hpsrv` | 同上 `--revert`（`userdel --remove`） |
| 系统包 `gcc libc6-dev make ripgrep` | `hpsrv` | 同上 `--revert`（按清单精确 purge） |
| rustup / Node / PATH 块 | `ccrun` 的 home | `p12-runtime-toolchain.sh --revert`（或随账号一起删） |
| 项目副本、bundle、`target/` | `ccrun` 的 home | 随 home 删除 |
| Runtime 配置里三个 workspace | `ccrun` 的 config.toml | 删这三段（随 home 一起删也算） |
| 本机 Claude Code 的 `mcpServers` 片段 | 本机 | 删那个条目 |
| 本机 `~/.local/bin/ccnm` | 本机 | 按用户意愿保留或用 `.pre-p12` 还原 |
| root 清单 `/var/lib/ccnm-p12-20260911` | `hpsrv` | 随 `--revert` 删除 |

## 六、判据与红线

- **unknown 不是绿。** 任何一项判不出来就算没通过，如实记进证据。
- **模型的话不是证据。** 判据是 Runtime 上的文件、属主、退出码、`git status`。
- **不为了让某一项过去放宽配置。** 不开 `allow_unconfined_exec`，不用 `--skip-identity-audit`（那个开关只给工具自己的离线自测），不把期望属主改成实际属主。
- **不碰已有账号和已有项目。** 本轮只用一次性资源；`/home/bing` 和 `/srv/git` 只读、不碰。
- **Linux 上第一次跑门禁红了就是发现。** 那是本轮最有价值的产出之一，按缺陷修，不改判据。
- **额度是有限的。** 第 5 步（不花额度）必须先通过，再进第 6 步。

## 七、执行之后

证据文件和一份脱敏记录写进 `docs/research/`，更新 `status.json` 的 P12。支持矩阵里 Remote Workspace MCP 那一项按结果决定是否摘掉 experimental；**如果真项目暴露了契约层面的问题，改契约而不是改结论。**
