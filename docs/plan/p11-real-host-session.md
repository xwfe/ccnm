# P11.3 真实 Host 会话计划

P11 的其余部分（P11.1、P11.2、P11.4）已经离线证完，记录见 [跨入口记录](../research/cross-entry-p11-2026-09-11.md)。**只剩这一件事需要真机**，而它需要用户逐项授权：重建 Runtime 身份与 SSH 准入、两端部署、用真实 Claude Code 连一次并消耗订阅额度。

这份文件把那一轮的顺序、判据、资源和清理先定下来。**写下来不等于授权执行**；执行前用户要明确说可以，其中三条还必须由用户本人 sudo。

## 一、这一轮要证明什么

| 要证明的 | 怎么算通过 |
| --- | --- |
| 真实 transport 上的允许矩阵 | `scripts/p11_matrix_check.py` 退出码 0，证据文件里 `passed: true`：read 四工具、coding 七工具、产物属主是 Runtime 执行身份、越权被拒、并发 coding 被拒、无私有路径泄漏 |
| 真实 Claude Code 能把它当 MCP server 用 | Claude Code 的 `/mcp` 看得到这个 server 和它的工具；read 模式下它读得到文件、写被拒；coding 模式下它改的文件在 Runtime 上真的变了 |
| Host 忽略 annotations 也越不了权 | read 模式下让它试着改文件或跑命令，结果是 `CCNM_E_POLICY`，且 Runtime 上确实没有那次写 |
| 启动失败对人可见 | 故意连一个没 opt-in 的 workspace，记录 Claude Code 那边**实际显示了什么**——`CCNM_E_*` 那行有没有到人眼前 |

**不证明的**：远端真实项目的长时 dogfood（那是 P12）、非 macOS、egress。

## 二、前置

### 用户做的（三条要 sudo，都在本机 bing 的终端里）

```bash
sudo /Users/bing/xdw/ccnm/scripts/p7-authorize-local-runtime.sh --apply
```

```bash
sudo /Users/bing/xdw/ccnm/scripts/p7-grant-local-ssh-access.sh --apply
```

```bash
sudo /Users/bing/xdw/ccnm/scripts/p7-isolate-local-runtime-group.sh --apply
```

顺序不能换（清单先建、再给准入、最后换主组），三条都是 P7 用过并归零过的脚本，`--revert` 互逆。**密码不要发给我。**

另外要用户确认的两件事：

- Agent 侧（`fodelf` 或本机）**已经登录 Claude Code**，且愿意让这一轮消耗额度；
- 允许把当前 build 部署到参与的机器上（替换前记版本和哈希，保留可回退副本）。

### 我做的（无特权）

- 生成本轮一次性 SSH 密钥对，只上传公钥；
- 建两个**一次性** workspace：`p11demo`（`external_mcp = "coding"`）和 `p11read`（`external_mcp = "read"`），root 都在 `/Users/Shared/ccnm-p11-<日期>/` 下，里面没有任何有价值的东西；
- 写 Claude Code 的 `mcpServers` 片段，指向 `ccnm mcp bridge`；
- 跑矩阵脚本、收证据、按逆序清理并逐条只读复核。

## 三、顺序

每一步做完停下来核对，不通过就不往下走。

### 0. 预检（只读，不改任何东西）

两端 `id`、`ccnm --version`、SSH 可达性、`ccnm doctor`。确认当前没有本轮资源残留。

### 1. 系统准备（用户 sudo）

第二节那三条。做完由我只读复核：`ccrun` 的 uid/gid、是否在 `com.apple.access_ssh`、home 权限、root 清单在不在。

### 2. 部署（获授权后）

把当前 build 装到参与的机器上；替换前记下原文件的哈希和路径，保留 `.pre-p11` 副本。

### 3. 配置一次性 workspace

在 Runtime 的权威配置里加 `p11demo` 与 `p11read`，root 指向本轮新建的空目录。**不碰任何已有 workspace。**

### 4. 脚本跑允许矩阵

在客户端机器上：

```bash
scripts/p11_matrix_check.py --workspace p11demo --node runtime --runtime-user ccrun --read-only-workspace p11read --out docs/research/p11-matrix-<日期>.json
```

判据：退出码 0，且证据里 `artifact_owner` 是 `ccrun`。**这一步不花额度**——脚本自己是 MCP 客户端，没有模型参与。它先跑是为了：链路有问题时在花钱之前就发现。

### 5. 真实 Claude Code（这一步花额度）

在 Claude Code 的 MCP 配置里加：

```json
{
  "mcpServers": {
    "ccnm-p11read": {
      "command": "ccnm",
      "args": ["mcp", "bridge", "p11read", "--node", "runtime", "--mode", "read"]
    },
    "ccnm-p11demo": {
      "command": "ccnm",
      "args": ["mcp", "bridge", "p11demo", "--node", "runtime", "--mode", "coding"]
    }
  }
}
```

在一次会话里做完三件事，每件都要记下 Claude 那边的原始显示：

1. **read 模式**：让它列目录、读文件（应成功），再让它改一个文件（应被拒）。拒绝之后去 Runtime 上确认**那次写确实没有发生**——模型说被拒了不算数，磁盘说了算。
2. **coding 模式**：让它改一个具体文件成一个具体内容。到 Runtime 上核对内容和属主。
3. **没 opt-in 的 workspace**：临时配一个指向 `private` 之类未开放 workspace 的 server，看 Claude Code 怎么显示这个起不来的 server，把原文记下来。

**验收一律看 Runtime 侧副作用与属主，不采信模型的自述。**

### 6. 清理并复核

按逆序撤销：一次性 workspace → 部署副本（按用户意愿保留或还原）→ 公钥 →
`p7-isolate-local-runtime-group.sh --revert` → `p7-grant-local-ssh-access.sh --revert` → `p7-revoke-local-runtime-key.sh --apply`（后三条用户 sudo）。然后逐条只读复核归零，无法清理的如实写进记录。

## 四、资源清单

执行时把实际值填进这张表，清理时逐条核对：

| 资源 | 在哪 | 怎么撤 |
| --- | --- | --- |
| 一次性 SSH 密钥对 | 客户端机器 `~/.ssh/ccnm-p11*` | 删文件 |
| 两端 `authorized_keys` 各一行 | 各自 home | 删那一行（备份在同目录） |
| `~/.ssh/config` 标记块 | 客户端机器 | 整块删除 |
| 一次性 workspace 目录 | `/Users/Shared/ccnm-p11-<日期>/` | 整个删除 |
| 配置里的 `p11demo` / `p11read` | Runtime 权威配置 | 删这两段 |
| Claude Code 的 `mcpServers` 片段 | Host 配置 | 删这两个条目 |
| 部署的二进制 | 各机器 `~/.local/bin/ccnm` | 按用户意愿保留或用 `.pre-p11` 还原 |
| `ccrun` 主组 / SSH 准入 / root 清单 | 本机 | 三条 `--revert` / `--apply`（用户 sudo） |

## 五、判据与红线

- **unknown 不是绿。** 任何一项判不出来就算没通过，如实记进证据。
- **模型的话不是证据。** 判据是 Runtime 上的文件、属主、退出码。
- **不为了让某一项过去放宽配置。** 例如产物属主不对就是不对，不能改成"期望属主 = 实际属主"再跑一遍。
- **不碰已有 workspace 和已有账号。** 本轮只用一次性资源。
- **额度是有限的。** 第 4 步（不花额度）必须先通过，再进第 5 步。

## 六、执行之后

把证据文件和一份脱敏记录写进 `docs/research/`，更新 `status.json` 的 P11：清掉 blocker、把 P11.3 加进 `completed_criteria`，并据结果决定支持矩阵里 Remote Workspace MCP 那一项还是不是 experimental。**如果真实 Host 暴露了契约层面的问题，改契约而不是改结论。**
