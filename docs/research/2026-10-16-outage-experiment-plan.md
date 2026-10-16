# 断网实验方案：MCP 连接能不能自己活过一次断网

> 状态：**2026-10-16 已执行（用户授权），结果见第 8 节：保活放宽没用，两头的 macOS 内核在约 100–200 秒就先把连接放弃了。** 第 0 节"断它的是 300 秒保活上限"的判断是错的，留着原文对照。

## 0. 要回答的问题

2026-10-15 项目机器（xdwmbp）Wi-Fi 断了约 390 秒，受管会话的工具通道断了。按代码，断它的是 AI 机器（fodelf）上那条 MCP ssh 自己的保活上限：`ServerAliveInterval=15` × `ServerAliveCountMax=20` = 300 秒没回音就退出（`crates/ccnm-core/src/ssh.rs` 的 `transport_options`）。项目机器这头只有写失败才收（[协议说明](../protocol/remote-workspace-mcp-v1.md) 第 6.2 节），两台之间走的是断网也不会变的 Tailscale 地址。

所以要测的只有一件事：**保活上限放宽以后，原来那条 TCP 能不能扛过 390 秒、10 分钟、20 分钟的断网；扛不过的话是哪一头、在第几秒先放弃的。**

- 扛得过：放宽保活上限（改一行）就能覆盖 10-15 那种事故，断网恢复方案（`docs/research/ccnm-mcp-recovery-plan-2026-10-16.md`，用户的草稿，未入库）里的常驻适配层先不做。
- 扛不过（某一头的内核先放弃重传）：放宽没用，才值得考虑适配层。
- 对照组在 390 秒时居然没断：说明我们对 10-15 的解释是错的，停下来重查。

## 1. 为什么不能直接关 Wi-Fi

xdwmbp 和 fodelf 之间现在有三条正在用的连接（2026-10-16 19:3x 实测）：

| 方向 | fodelf 端口 | 是什么 |
| --- | --- | --- |
| fodelf → xdwmbp:22 | 62231 | **这个对话本身**（桌面端经 SSH 起的会话，进程树里有 `~/.claude/remote/.../server --bridge`） |
| fodelf → xdwmbp:22 | 50684 | workspace `shun` 正在用的 MCP 通道（`ccnm internal mcp-serve`） |
| xdwmbp → fodelf:22 | — | 本机发起的另一条 ccnm MCP 通道 |

关 Wi-Fi 会把这三条一起断掉。所以实验**只挡新开的两条测试连接**：用 macOS 自带的防火墙 pf，按 fodelf 那头的源端口丢包。在 TCP 看来这和网断了一样，都是包发出去没有回音、两头各自重传；别的连接一个包都不受影响。

这样模拟不到的：Wi-Fi 网卡掉线再连上、Tailscale 换路由、底层 IP 变化。两头的 TCP 端点（100.x 地址）不变，所以 TCP 的行为一样；要连这些一起验，只能等哪天你手头没有要保住的会话时，手动关一次 Wi-Fi。

## 2. 两条测试连接（在 fodelf 上，我来跑，不需要 sudo）

参数和真实 MCP 通道逐项一致（`ssh.rs` 第 721 行那条测试里的完整命令），只有 B 的 `ServerAliveCountMax` 不同，远端跑的不是 ccnm，而是一个每 30 秒写一行时间的循环——和项目机器上 `mcp-serve` 空闲时每 30 秒写一次 ping 一样，只有写失败才退出。

| 连接 | 保活 | 预期 |
| --- | --- | --- |
| A（对照） | 15 秒 × 20 = 300 秒 | 断 390 秒时在 300–315 秒之间退出，报 `Timeout, server not responding` |
| B（候选） | 15 秒 × 240 = 60 分钟 | 要测的就是它 |

```sh
#!/bin/sh
# fodelf: /tmp/ccnm-outage/client.sh，放在单独的 tmux server（-L ccnm-outage）里跑，
# 不碰 ccnm 自己的 tmux。
set -uf   # -f：$opts 里的 SendEnv=-* 不展开成文件名
dir=/tmp/ccnm-outage
mkdir -p "$dir"
opts='-o SendEnv=-* -o SetEnv=CCNM_TRANSPORT=1 -o ForwardAgent=no -o ClearAllForwardings=yes
      -o BatchMode=yes -o ConnectTimeout=10 -o ControlMaster=no -o ControlPath=none
      -o ServerAliveInterval=15 -o SendEnv=-ANTHROPIC_* -o SendEnv=-CLAUDE_*'
conn() {
  name=$1 count=$2
  { ssh $opts -o ServerAliveCountMax="$count" -T xdwmbp \
      "/bin/sh -c 'while :; do date +%s; sleep 30; done' ccnm-outage-$name" 2>"$dir/$name.err"
    echo "ssh_exit=$? at $(date +%s)" >>"$dir/$name.err"
  } | while IFS= read -r line; do echo "$(date +%s) $line"; done >"$dir/$name.log"
}
conn A 20 &
conn B 240 &
# fodelf 这头的 socket 状态，每 30 秒一次
while :; do echo "$(date +%s)"; netstat -anp tcp | grep '100.107.211.119.22 ' ; sleep 30; done >"$dir/netstat.log" &
wait
```

起好以后我在 xdwmbp 上用 `lsof` 找出这两条连接在 fodelf 那头的源端口，交给你。

## 3. 断网脚本（在 xdwmbp 上，**你在自己的终端里 `sudo` 跑一次**）

两台的 sudo 都要密码，我这边的 shell 拿不到，所以这一步只能你来。跑一次，约 45 分钟，四轮自动断开、自动恢复：

| 轮 | 断多久 | 看什么 |
| --- | --- | --- |
| R0 | 45 秒 | 规则真的挡住了（两条连接的日志在这段时间里停住，之后补上）；别的会话没受影响 |
| R1 | 390 秒 | 10-15 那次的时长：A 应该断、B 是关键 |
| R2 | 600 秒 | B 还在的话再拉长 |
| R3 | 1200 秒 | B 还在的话找内核的上限 |

每轮恢复后等 120 秒，看连接是不是真的接上了。哪条连接已经断了，后面的轮次就不再挡它；两条都断了就提前结束。

```sh
#!/bin/sh
# 用法：sudo sh outage-pf.sh <A 的端口> <B 的端口>
# 只挡 fodelf 这两个源端口和本机 22 之间的包，别的连接一概不碰。
set -u
FODELF=100.79.121.33 SELF=100.107.211.119 ANCHOR=com.apple/ccnm-outage
LOG=/tmp/ccnm-outage-pf.log
PA=$1 PB=$2
log() { echo "$(date +%s) $*" | tee -a "$LOG"; }

# 端口必须属于一条 fodelf -> 本机:22 的已建立连接，而且那条连接底下跑着测试标记。
# 写错端口（比如写成这个对话那条）就拒绝，不会去挡。
ours() {
  for pid in $(lsof -nP -t -iTCP@$FODELF:"$1" -sTCP:ESTABLISHED 2>/dev/null); do
    ps -ax -o pid=,ppid=,command= | awk -v p="$pid" '$1==p || $2==p' | grep -q 'ccnm-outage-' && return 0
  done
  return 1
}
for p in "$PA" "$PB"; do ours "$p" || { log "端口 $p 不是测试连接，什么都没做"; exit 1; }; done

token=$(pfctl -E 2>&1 | awk '/Token/ {print $3}')
# 兜底：这个脚本被 kill -9 也一样，55 分钟后无条件清掉规则
nohup sh -c "sleep 3300; pfctl -a $ANCHOR -F rules" >/dev/null 2>&1 &
deadman=$!
( while :; do log "netstat $(netstat -anp tcp | grep -E "\.($PA|$PB) " | tr -s ' ' | tr '\n' ';')"; sleep 30; done ) &
watcher=$!

restore() { pfctl -a "$ANCHOR" -F rules >/dev/null 2>&1; log "已恢复"; }
finish() {
  restore
  [ -n "$token" ] && pfctl -X "$token" >/dev/null 2>&1
  kill "$watcher" "$deadman" 2>/dev/null
  log "结束，pf 引用已释放"
}
# Ctrl-C、关终端、kill 都先退出，退出时统一清规则；只挂 EXIT 一处，不会清两遍
trap finish EXIT
trap 'exit 130' INT TERM HUP

for secs in 45 390 600 1200; do
  alive=""
  for p in "$PA" "$PB"; do ours "$p" && alive="$alive $p"; done
  [ -z "$alive" ] && { log "两条都断了，提前结束"; break; }
  ports=$(echo $alive | tr ' ' ',')
  printf 'block drop quick proto tcp from %s port { %s } to %s port 22\nblock drop quick proto tcp from %s port 22 to %s port { %s }\n' \
    "$FODELF" "$ports" "$SELF" "$SELF" "$FODELF" "$ports" | pfctl -a "$ANCHOR" -f - 2>>"$LOG"
  log "开始挡 $secs 秒：$ports"
  sleep "$secs"
  restore
  sleep 120
done
```

**安全兜底有三层：** 每轮到点恢复；脚本退出、Ctrl-C、关终端都会清规则（`trap`）；被强杀的话 55 分钟后后台那个兜底进程也会清。规则只放在 `com.apple/ccnm-outage` 这个锚点里，不改 `/etc/pf.conf`（本机的 `pf.conf` 已经有 `anchor "com.apple/*"`）。`pfctl -E/-X` 用引用计数开关 pf，结束时恢复到原来开或关的状态。

**R0 没看到日志停住**（比如 Tailscale 的包没经过 pf，或者 pf 里已有的连接状态让包绕过了规则）就说明这种挡法无效：直接 Ctrl-C，什么都没发生，换方法再议，不要硬改规则。

## 4. 记什么

| 记什么 | 从哪来 |
| --- | --- |
| 每轮开始挡、恢复的时刻 | xdwmbp `/tmp/ccnm-outage-pf.log` |
| 本机这头两条 socket 每 30 秒的状态（发送队列有没有在涨、什么时候消失） | 同上，`netstat` 行 |
| 两条连接每次收到一行的时刻（断开期间应该停住，恢复后补上） | fodelf `/tmp/ccnm-outage/A.log`、`B.log` |
| ssh 客户端什么时候退出、退出码、报错原文 | fodelf `A.err`、`B.err` |
| fodelf 那头 socket 的状态 | fodelf `netstat.log` |
| 两台系统版本 | xdwmbp macOS 26.6.2，fodelf 26.3 |

结论按第 0 节的三种情况下，原始日志脱敏后附在本文末尾。

## 5. 收尾

- fodelf：`tmux -L ccnm-outage kill-server`，删 `/tmp/ccnm-outage`。
- xdwmbp：`sudo pfctl -a com.apple/ccnm-outage -s rules` 应该是空的；`/tmp/ccnm-outage-pf.log` 留到结果写进文档后再删。
- 不动 ccnm 二进制、配置、正在跑的会话，不花模型额度。

## 6. 第二步（这次不授权，只是说清楚之后要做什么）

只有 B 扛过了 390 秒才做：

1. 按 R1–R3 测出的上限选一个保活值（比上限留出余量），改 `transport_options`，加测试。
2. 用这个构建在测试 workspace 上起一个真实受管会话，用同样的 pf 方法只挡它的 MCP 通道 390 秒，恢复后看 `ccnm status` 显示工具"通"、项目机器上 `mcp-serve` 还是原来那个 pid、Claude 的 `/mcp` 显示已连接；最后让模型调一次 `workspace_info` 当实证（会花一点额度）。
3. 这一步要换 fodelf 上的 ccnm，属于部署，到时另外授权。

## 7. 要你批的

1. 我在 fodelf 上开两条测试 ssh 连接（单独的 tmux server，远端只跑写时间的循环），日志写 `/tmp/ccnm-outage`，做完清掉。
2. 你在本机终端里用 `sudo` 跑一次第 3 节的脚本（端口我起好连接后给你），约 45 分钟，跑的时候这个对话和 `shun` 会话照常不受影响。

## 8. 结果

2026-10-16 20:24–20:35 JST 执行。

**结论：属于第 0 节的第二种情况。断网时连接上有数据在等确认，macOS 内核在 100–200 秒内就放弃这条 TCP，两头都是；保活上限放到 60 分钟的 B 和 300 秒的 A 几乎同时死。放宽 `ServerAliveCountMax` 解决不了 10-15 那种事故。**

### R0（45 秒）：挡法有效

挡住期间两条连接都收不到行，远端 20:24:11、20:24:41 写的两行在恢复后 1–2 秒内一起到达，之后照常每 30 秒一行。说明 pf 挡得住走 Tailscale（utun4）的包，且挡的只是这两条：同一时间这个对话（fodelf 源端口 62231）、`shun` 的 MCP 通道（50684）都没断，实验结束后三条既有连接仍在。

### R1（390 秒）：两条都死在 200 秒以内

从开始挡算起：

| 时刻 | xdwmbp（服务端这头） | fodelf（ssh 客户端） |
| --- | --- | --- |
| +0 s | 开始挡 | |
| +42 s 起 | 两条的发送队列每 30 秒涨 52 字节（远端循环写的那一行，发不出去） | |
| +103–133 s | A 的 socket 消失 | |
| +163–193 s | B 的 socket 消失 | |
| +194 s | | A 退出 255：`Read from remote host xdwmbp: Operation timed out` |
| +199 s | | B 退出 255：同一句 |
| +390 s | 恢复，两条早已不在 | |

R2、R3 没跑：脚本检查到两条都断了，提前结束、清了规则、释放了 pf 引用。

两头的报错都是内核给的 `Operation timed out`（重传超时，`ETIMEDOUT`），不是 ssh 自己保活到点时那句 `Timeout, server not responding`。A 的保活要 300 秒才到点、B 要 60 分钟，都没走到。

### 这说明什么

- **断网期间两头都在发东西，所以内核一定会放弃。** 项目机器上的 `mcp-serve` 空闲时每 30 秒写一次 ping，AI 机器上的 ssh 每 15 秒发一次保活；这些数据等不到确认，内核按退避重传，到点就关连接。macOS 上这个点在 100–200 秒（各条连接的往返时间不同，退避起点不同，所以 A、B 在服务端差了约 60 秒）。
- **10-15 那次（390 秒）的连接，几乎可以肯定也是这样在 3 分钟左右死的**，不是 300 秒保活。`ssh.rs` 里"睡两分钟、换 Wi-Fi、路由器重启，TCP 都活得过来"那句注释只在断开不到约 100 秒时成立，已按本次结果改。
- **想靠"不放弃"保住原连接，只能断网时两头都不发东西**：拿掉 Runtime 的 ping（它是半开检测，写锁靠它在对面没了时释放）和 ssh 保活，而且模型在断网期间一调工具照样有数据在途。不可取。
- **所以断网超过约 2 分钟，MCP 通道必断，要恢复只能重新建连接。** 断网恢复方案（`docs/research/ccnm-mcp-recovery-plan-2026-10-16.md`，用户的草稿，未入库）里"重连"这个方向是对的；之前建议的"先放宽保活"作废。

### 没测到的

- Linux 当项目机器时：Linux 默认 `tcp_retries2=15`，放弃得更晚（量级 15 分钟），但 AI 机器是 macOS 时客户端这头照样约 3 分钟就放弃。没在 Linux 上量。
- 真关 Wi-Fi（网卡掉线、Tailscale 换路由）：两头的 TCP 端点不变，行为应当相同；没测。
- 测试脚本两处小毛病，不影响结论：远端循环在 ssh 断后不退（`date` 写失败不会让 `sh` 退出，ccnm 的 `mcp-serve` 是写失败就退的），实验后手动收掉；兜底进程只杀掉了外层 `sh`，留下一个 root 的 `sleep 3300` 到点自己退出，它已经没有后续动作。
- 原始日志（时间戳、netstat 行）存在本次会话的临时目录，没进仓库；上表就是全部用到的数据。
