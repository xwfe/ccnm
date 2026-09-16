# P15：外部入口配置示例启用 alwaysLoad（2026-09-16）

## 结论

- Claude Code 默认把 MCP 工具放进**延迟加载池**：工具表里只留名字，模型要先调一次内置的 `ToolSearch` 才拿得到 schema。ccnm 的外部入口（用户自己的 Claude Code 接 `ccnm mcp bridge`）因此每个任务白多一个回合。
- 在 `mcpServers` 里给这个 server 写 `"alwaysLoad": true`，它的工具首轮就进工具表。本机零额度复现：延迟池 `coding` 模式 21 → 14、`read` 模式 18 → 14，少掉的正好是 ccnm 的 7 个 / 4 个。
- 代价是 Claude Code 会把它排进「首轮请求前必须连上」的那一组。bridge 要 ssh 到 Runtime，Runtime 不可达时启动会卡在这里。
- **Managed 路径不需要改**：那条路启动 Claude Code 时传 `--tools ""`，`ToolSearch` 本身不可用，七个工具一直全量加载。
- **没验到的**：模型实际行为（本机 CLI 未登录，四次连接都没发出模型请求；收益数据来自 workspace-kernel 的 V2-Q2，见下）；`.mcp.json` / `~/.claude.json` 这两种放法（实测走的是 `--mcp-config` 文件，同一份 server 配置 schema）；除 Claude Code 外的任何 Host。

## Host 行为依据

打包在本机 `claude` 二进制里的 JS（2.1.269，`GIT_SHA d0733697`）。**是否延迟**：

```js
function SY(e){if(e.alwaysLoad===!0)return!1;if(Tt(e))return!1;
  if(e.isMcp===!0)return!Xye();return e.shouldDefer===!0}
```

`alwaysLoad` 为真就直接不延迟，不看别的条件。工具对象上的这个标记来自 server 配置（`serverAlwaysLoad:e.config.alwaysLoad===!0`）。

**连接分组**（同一份二进制）：

```js
Re=(qe,Ye)=>qe.alwaysLoad===!0||te!==void 0&&Object.hasOwn(te,Ye),
De=zi(P,Re),xe=zi(P,(qe,Ye)=>!Re(qe,Ye)),
…Gd(!1,()=>Ma(De,"regular-required",U,!1,…),"--mcp-config alwaysLoad servers")
…Gd(we,()=>Ma(xe,"regular",U,Pe,…),"--mcp-config servers")
```

`we` 是 `MCP_CONNECTION_NONBLOCKING !== "false"`，默认真。所以带 `alwaysLoad` 的那组是**阻塞连接**（第一个参数写死 `!1`），其余非阻塞。这就是上面那条「代价」的出处——不是猜的。

工具自己的 `_meta["anthropic/alwaysLoad"]` 也能达到同样的免延迟效果，但不进这个连接分组（依据在 workspace-kernel 仓库 `evidence/v2-q2/README.md`）。P15 没走这条：模型对照测的是配置那条，`_meta` 没有实测数据，而且 ccnm 这种「不连上就干不了活」的 server，等连接是想要的行为。

## 本机复现（零额度）

macOS arm64，Claude Code 2.1.269，ccnm 0.7.0（`~/.local/bin/ccnm`，release 产物）。脚本是 workspace-kernel 仓库的 `evidence/v2-q2/defer_check.sh`，ccnm 这边不复制第二份；`read` 那两格是把它的 `mode` 和 `external_mcp` 换成 `read` 跑的。

它做的事：临时目录里放一份本机 Runtime 配置（workspace `demo`、`allow_unconfined_exec = true`），MCP 命令用 `/usr/bin/env -i` 起 `ccnm internal mcp-serve`（不 `env -i` 会撞上 `CLAUDE_CODE_MESSAGING_TOKEN` 被当成继承凭据，原因见 [P13 的记录](p13-instructions-host-cap-2026-09-16.md)），再 `env -i` 起 `claude -p --strict-mcp-config --mcp-config … --debug-file …`。**不传 `--tools ""`**，让 `ToolSearch` 可用，这样才是外部入口的形状。

| 模式 | 配置 | `Dynamic tool loading` | 连接耗时 | 结果 |
| --- | --- | --- | --- | --- |
| `coding`（7 工具） | 不写 `alwaysLoad` | `0/21 deferred tools included` | 507 ms | `Not logged in`，`input_tokens` 0 |
| `coding` | `"alwaysLoad": true` | `0/14` | 178 ms | 同上 |
| `read`（4 工具） | 不写 `alwaysLoad` | `0/18` | 181 ms | 同上 |
| `read` | `"alwaysLoad": true` | `0/14` | 216 ms | 同上 |

21−7 = 18−4 = 14，两式对上：剩下的 14 是 Claude Code 自己的内置延迟工具，跟 ccnm 无关。四次都在认证阶段停下，没发模型请求。连接耗时是本地 Runtime（不过 ssh），不代表真实部署。

## 模型侧的收益（引用，不在本仓重跑）

来自跨仓计划 workspace-kernel 的 V2-Q2（`evidence/v2-q2/README.md`），2026-09-16 在 Agent 机器上跑：3 个任务 × 2 组 × 3 次 = 18 格，Claude Code 2.1.272 + ccnm 0.7.0，`--model sonnet`，每格全新夹具。

18 格全部通过，两组成功率都是 3/3。差别只在过程：不写 `alwaysLoad` 的一组**每一格都恰好调一次 `ToolSearch`**，然后工具序列跟另一组一模一样；写了的一组一次都不调，每个任务少一个回合，墙钟和总输入 token（含 cache）都不更差。判据是实验前冻结的，四条全满足，所以采纳。

那一轮消耗订阅额度 20 次、$2.1454，记在 workspace-kernel 的授权账本里。**ccnm 这一侧没有再花额度**，本阶段只改文档。

## 怎么复跑

在有 ccnm 和 Claude Code 的机器上（登录与否都行，未登录反而更干净）：

```bash
bash <workspace-kernel>/evidence/v2-q2/defer_check.sh ~/.local/bin/ccnm /tmp/defer-out false
bash <workspace-kernel>/evidence/v2-q2/defer_check.sh ~/.local/bin/ccnm /tmp/defer-out true
```

看输出里的 `Dynamic tool loading: 0/N`。输出目录别放仓库里。Claude Code 换版本后这个数字会变（内置延迟工具增减），有意义的是**同一版本下两次之间的差**。
