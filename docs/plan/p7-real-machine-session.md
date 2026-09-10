# P7.3 真机会话计划

这是 P7.3 执行前的准备文档，**不是授权记录**。里面每一个特权动作都要用户单独批准；批准执行也不等于当前进程有管理员权限，密码只能输入两端操作系统自己的认证界面，不发送给 Agent。

写它的原因很直接：P3 那次同样的环境来回了十几轮，而这一次要在同一套环境里做完三件事。先把顺序、判据和清理路径定下来，能省掉的就是真金白银的订阅额度。

## 一、这一次要产出什么

三件事，一套环境，顺序不能换：

1. **原 P6.3 的内容**（已并入 P7.3）：用公共 API 各跑一次真实 provider 的双机闭环，与人类 CLI 结果对照。**协议 v1 目前只是候选，要靠这一步的结果才能确立或修改。**
2. **真实项目的完整 dogfood**：启动 → 修改 → 测试 → 结果 → 停止/恢复，走完一整圈，外加生产边界复核。
3. **全部门禁重跑**：Rust、Python、计划检查、契约测试，失败、跳过和未测平台如实列出。

第 1 步失败就不要进第 2 步。协议还没定，拿它去做 dogfood 只会得到一份要重做的记录。

## 二、需要单独授权的动作

| 对象 | 动作 | 怎么撤销 |
| --- | --- | --- |
| fodelf | 生成一次性 SSH 密钥，**私钥不离开 fodelf** | 删掉那两个文件 |
| 本机 ccrun | 把本轮公钥追加进 `authorized_keys` | 只删本轮那一行，其他行原样保留 |
| 本机 ccrun | 加入 `com.apple.access_ssh` 直接成员 | 只删这一个直接成员关系 |
| 本机 ccrun | 主组从 `staff` 换成专用组 | 换回 `staff`，删专用组 |
| fodelf | SSH config 追加本轮唯一 alias，指回本机的 ccrun | 只删本轮区块，不整份恢复备份 |
| 两端 | 部署本轮构建的 ccnm，重启 Controller | 装回原来的版本 |
| 订阅 | 真实 Claude / Codex 执行，消耗额度 | **不可撤销** |

最后一行不可撤销，所以对照那一步一次跑对比跑三次省钱——这正是 `scripts/p7_parity_check.py` 存在的理由。

**不需要创建任何新账号。** 本机的 `ccrun` 是既有账号，只改它的主组和准入，不重建、不动 UID/shell/密码。因此第五节那套删账号的坑这一轮碰不到，但清理仍然照清单逆序做。

## 三、拓扑：已定

**Agent = fodelf（Mac mini），Runtime = 本机。** 用户确认两个 provider 的 CLI 都登录在 fodelf 上，所以只有这一个方向可行——也正是 P3 跑 Claude 时验证过的那个。

好处是**项目留在本机**：dogfood 直接用真实仓库，不用在对端另放一份。

P3 是两个相反方向各跑一次（Claude 在 fodelf 当 Agent、Codex 在本机当 Agent），要两套特权准备。这一轮合成一个方向，本机 ccrun 那三步做一次就够，fodelf 上一个账号都不用建。

### 开工前的基线（2026-09-10 只读复核）

P3 的清理确实归零了，三步都要重做：

| 检查 | 结果 |
| --- | --- |
| `ccrun` | uid=504，gid=20(staff) |
| `com.apple.access_ssh` | 不是成员 |
| 组名 `ccrun` / GID 504 | 空闲 |
| `/var/db/ccnm-p3-local-20260908` | 已删 |
| `/Users/ccrun` | `ccrun:staff` 0700 |
| ccrun 的进程 | 0 个 |

最后一行要在换主组之前再确认一次：**改主组不影响已经在跑的进程的组身份**，留着会让验收结果对不上。有残留就先 `sudo launchctl bootout user/504`，脚本不会自动 kill。

## 四、执行顺序与判据

每一步的证据都要落到文件，**不能只留在终端里**——终端一关就没了，而 P7.4 要拿这些做发布判断。

### 0. 只读预检

两端 `id`、`ccnm --version`、两个 CLI 的登录状态、SSH 可达性。确认当前没有任何本轮资源残留。这一步不改任何东西。

### 1. 建环境

按第二节的表逐项执行，每执行一项就写进 root 清单（`/var/db/ccnm-p7-<日期>`，0700）。清单是清理的唯一依据：**没记进清单的东西，清理时一律不删**。

### 2. 部署

两端装**同一个 build**，用 `scripts/deploy.sh`。别 `cp` 覆盖正在跑的二进制——原因和症状见[运维手册](../operations.md)。装完两端 `ccnm doctor` 必须 PASS。

### 3. 对照（原 P6.3）

每个 provider 跑一次：

```bash
scripts/p7_parity_check.py \
    --workspace <ws> --root <工作树> \
    --instance claude-main --provider claude \
    --guard-dir <Runtime 执行身份的 state>/ccnm/write-guards \
    --out docs/research/p7-parity-claude.json
```

它把同一件事分别走人类 CLI 和 machine API，然后比**副作用**而不是模型说了什么：两条腿各自在工作树里写一个带一次性 token 的文件，脚本回头去看文件在不在、内容对不对、属主是谁。七项检查里任何一项判不出来都不算通过。

**`--guard-dir` 别省。** 工作树的写入 guard 由 Runtime 侧的 MCP 进程持有，进程退出才释放；而 `ccnm run --print` 是走另一条 SSH 通道同步返回的，两者之间没有任何同步。第一条腿刚返回就起第二条，可能撞上 guard 还锁着——报出来是"machine API 失败"，实际只是没排开，而这一撞就是一次额度。给了目录它就等到真放开，不给只能盲等一段固定时间，证据里的 `sequencing.method` 会写明这一步是观察到的（`guard-dir`）还是假设的（`fixed-delay`）。

目录是**Runtime 执行身份**的 `${XDG_STATE_HOME:-~/.local/state}/ccnm/write-guards/`，不是操作者自己那份。读不到就退回盲等，不要为了读它去放宽权限。

开跑前那个目录里如果已经有 `held` 记录，脚本会直接停下——那时候起腿注定失败，先按[运维手册](../operations.md)的写入 guard 残留一节处理。

判不出和没通过要原样记进 `docs/research/`，**不要重跑到绿为止**——每一轮都在花额度，而反复重试掩盖掉的正是要找的问题。

跑完看两份证据文件，据此确立协议 v1 或者改它。改了协议就要回头更新 schema、fixture 和兼容规则，那三样是一起的。

### 4. 完整 dogfood

用真实项目走一圈：启动 → 让 Agent 改点东西 → 跑测试 → 取结果 → 停止 → 恢复。P3 已经覆盖过 detach/reattach、Controller 重启、精确 stop 和 transport 故障，**这一步不用重做那些**，重点是"一个真实任务从头到尾能不能交付"。

同时做生产边界复核：Runtime 身份读不到 Agent 凭据、拿不到 sudo、写不了特权 socket。P3 的检查方法可以照搬。

### 5. 门禁

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 -m unittest discover -s tests -p 'test_*.py' -q
python3 scripts/check_protocol.py
python3 scripts/check_plan.py
```

clippy **不要接进管道**——`| tail` 会吞掉它的退出码，上一轮就这么提交过一次失败的 clippy。

### 6. 清理并复核

见第五节。清完两端逐项只读复核归零，然后才写 status.json。

## 五、清理：P3 撞出来的四堵墙

删账号这一步 P3 连续失败三次才成。`scripts/p3-cleanup-runtime-user.sh` 里已经把结论写进代码，本轮的清理脚本照抄这四条，别再撞一遍：

1. **`/Users` 带 `sunlnk` 标记**（system no-unlink），连 root 都不能删除其中的条目。`rmdir /Users/<账号>` 报 `Operation not permitted`，看着像权限不够，实际是文件标记。
2. **`chflags nosunlnk /Users` 也会被拒**——`/Users` 列在 SIP 的 `/System/Library/Sandbox/rootless.conf` 里。这条路走不通，删账号改用 `sysadminctl -deleteUser`，它带 SIP entitlement。顺带一提：`/var/db` 也带 `sunlnk`，但它**可以**临时摘掉，两者不一样，别一概而论。
3. **`sysadminctl -deleteUser` 会连主组一起删掉。** 之后再 `dscl . -delete /Groups/<名字>` 会报 `Invalid Path` / `DS Error -14009`，那不是失败，是已经没了。删组前先判断它还在不在。
4. **删目录内容之前，先确认这个目录最后删得掉。** P3 就吃过这个亏：清单内容删干净了，却留下一个删不掉的空目录，卡在最难看的中间状态。`rmdir` 对非空目录报 `Directory not empty`、对删不掉的父目录报 `Operation not permitted`，两者可区分，所以这是一次无损探测。

另外两条同样是实测出来的：

- **`sysadminctl` 失败时也可能返回 0**，只认实际结果，别信退出码。删完 `dscacheutil -flushcache` 再复核。
- **清理脚本必须可重跑。** P3 那版所有前置检查都假设账号还在，账号删掉之后就再也跑不起来，剩下的组和清单只能手工收尾。

## 六、什么时候停下来报告

- 特权动作的前置核对对不上（名字、UID、属主、权限任意一项）：停，不覆盖、不猜。
- 发现清单之外的残留：停，等人工判断，不递归删。
- 对照工具给出 `fail` 或 `inconclusive`：记录原样，不重试到绿。
- 隔离检查发现 Runtime 能读到 Agent 凭据：停，这是安全结论，不是可以事后补的细节。
- 清理有任何一项做不掉：如实写进"无法清理项"，不假装归零。

## 七、这份计划本身没有验证过什么

它是照着 P3 的记录和当前代码写的。第四节里除了 `p7_parity_check.py`（自身有 30 个离线测试）之外，其余命令本轮都没有在真机上执行过。第三节关于"一个方向够不够"的判断依赖开工时的只读预检，现在只是待确认的假设。
