# 把家庭机变成一个可以放心跑 exec_command 的地方

**先说结论：`exec_command` 是一个远程 shell。** 它能跑的东西，等于 ccnm runtime 所在那个
账号能跑的东西——包括 `cat ~/.ssh/id_ed25519`、`curl -d @secrets ...`、`rm -rf ~`。

ccnm 自己**做不到**限制这件事，也不打算假装能做到。设计文档第 18 节的原话是
"command parser 不是 sandbox"。真正的边界是操作系统给的：一个专用 Unix 账号，只能碰这一个项目，
没有 sudo、没有 ssh key、没有 Claude 凭证、没有浏览器 profile。

ccnm 能做的只有两件：**验证**这些性质成不成立，以及在不成立时**拒绝**跑命令。

```text
ccnm doctor <workspace>      看每一条性质当前是什么状态
exec_command                 不满足就直接拒绝，除非你显式说了接受
```

本文是你要在**家庭机**（也就是 runtime 所在那台）上手动做的事。ccnm 一条都不会替你做——
创建用户、改权限这种事，一个诊断工具不该背着你干。

---

## 0. 先看现在是什么状态

```bash
ccnm doctor <workspace>
```

关注这几行。它们就是下面每一节要解决的：

```text
Runs as root            runtime 不能是 root
Runtime user            必须是你在 config.toml 里声明的那个专用账号
No sudo                 不能免密 sudo
Not an admin            不能在 admin / wheel / sudo 组里
No SSH keys             这个账号的 ~/.ssh 里不能有可读的私钥
No Claude credential    这台机器不能有 Claude 凭证（核心 invariant）
No Docker socket        不能写 /var/run/docker.sock（那等于 root）
exec_command            上面全过才是 "confined"
```

---

## 1. 建一个专用账号 `ccrun`

macOS 上没有 `useradd`。下面这些命令**需要 sudo，需要你自己敲**，我没有在你的机器上跑过它们
（跑了就等于我替你建了个用户）。

先挑一个没被占用的 UID：

```bash
dscl . -list /Users UniqueID | awk '{print $2}' | sort -n | tail -1
```

假设最大是 501，那就用 502：

```bash
sudo dscl . -create /Users/ccrun
sudo dscl . -create /Users/ccrun UserShell /bin/zsh
sudo dscl . -create /Users/ccrun RealName "ccnm runtime"
sudo dscl . -create /Users/ccrun UniqueID 502
sudo dscl . -create /Users/ccrun PrimaryGroupID 20          # staff
sudo dscl . -create /Users/ccrun NFSHomeDirectory /Users/ccrun
sudo mkdir -p /Users/ccrun
sudo chown -R ccrun:staff /Users/ccrun
sudo chmod 700 /Users/ccrun
```

**不要**把它加进 `admin`：

```bash
dscl . -read /Groups/admin GroupMembership          # ccrun 不该出现在这里
```

它需要能被 ssh 进来（工作机启动 runtime 走的就是 ssh）：

```bash
sudo mkdir -p /Users/ccrun/.ssh
sudo chmod 700 /Users/ccrun/.ssh
# 把工作机的公钥放进去 —— 只放公钥，永远不放私钥
sudo tee /Users/ccrun/.ssh/authorized_keys < /path/to/work-machine.pub
sudo chown -R ccrun:staff /Users/ccrun/.ssh
sudo chmod 600 /Users/ccrun/.ssh/authorized_keys
```

`No SSH keys` 那条检查的是**私钥**：它会读 `~/.ssh` 里每个文件的开头找 `PRIVATE KEY`。
`authorized_keys`、`known_hosts`、`.pub` 都不算。

装 ccnm 和它需要的工具：

```bash
sudo -u ccrun mkdir -p /Users/ccrun/.local/bin
sudo cp target/release/ccnm /Users/ccrun/.local/bin/ccnm
sudo chown ccrun:staff /Users/ccrun/.local/bin/ccnm
# search_text 需要 rg。Homebrew 装的对所有用户可见，不用重装
```

在 config.toml 里声明它：

```toml
[hosts.home]
ssh_from_work = "ccnm-home"
runtime_user  = "ccrun"        # ← 这一行
```

**不声明 `runtime_user` 本身就是一条失败。** 没有它，ccnm 分不出"这是专用账号"和
"这是开发者自己的账号"，而这时候回答"看起来没问题"是所有答案里最糟的一个。

---

## 2. 只给它这一个项目

默认情况下 `ccrun` 读不到 `/Users/你/` 下面的东西（macOS 家目录是 700），但它也读不到
你的项目。用 ACL 精确开一个口子：

```bash
PROJ=/Users/你/code/你的项目

# 让 ccrun 能穿过路径上的每一层目录（只是 execute，不是 read）
chmod +a "user:ccrun allow execute" /Users/你 /Users/你/code

# 项目本身给读写，并且让新建的文件继承这条规则
chmod -R +a "user:ccrun allow \
list,search,add_file,add_subdirectory,delete_child,readattr,writeattr,\
readextattr,writeextattr,readsecurity,file_inherit,directory_inherit" "$PROJ"
```

验证一下（这条我在本机验过语法，ACL 确实生效）：

```bash
ls -lde "$PROJ"                       # 应该看到 user:ccrun allow ...
sudo -u ccrun ls "$PROJ"              # 能列
sudo -u ccrun ls /Users/你            # 应该 Permission denied
sudo -u ccrun cat /Users/你/.ssh/id_ed25519   # 必须 Permission denied
```

最后一条是这一整节的意义所在：**穿得过去，但只能到项目那一层。**

> 想撤销：`chmod -R -a "user:ccrun allow ..." "$PROJ"`，或者 `chmod -R -N "$PROJ"`
> 清掉全部 ACL（会连你自己加的其它 ACL 一起清）。

---

## 3. 不给 sudo

`ccrun` 不在 `admin` 组就已经没有 sudo 了。确认一下：

```bash
sudo -u ccrun sudo -n true          # 应该失败
```

如果你的 `/etc/sudoers.d/` 里有什么通配规则，检查它没有覆盖到 `ccrun`。

---

## 4. 这台机器不能有 Claude 凭证

这是第 6 节的核心 invariant，不是建议：**家庭机不持有 Claude 凭证，也不能成为 Anthropic 的推理出口。**

`ccrun` 的家目录是全新的，所以默认就没有。要保证的是不要在这台机器上做这两件事：

```bash
claude auth login          # 永远不要在家庭机上跑
claude                     # 也不要，即使只是想试试
```

如果 `ccnm doctor` 报 `No Claude credential FAIL`，说明**当前 runtime 账号的家目录里有
`.claude/.credentials.json`**。如果你还在用自己的账号跑 runtime（也就是还没做完第 1 节），
那报的就是你自己那份——这正是为什么要有专用账号。

---

## 5. 出网限制

设计文档第 19 节把这条写成**有条件**的：

> 如果这条网络出口约束是绝对合规边界，Production gate 要求 ccrun 账户 / 执行沙箱没有公网
> egress，或至少由 OS / network policy 阻断 Anthropic。**不要把静态 command deny 写成
> "网络安全边界"。**

所以 `ccnm doctor` 对这条只报 WARN，判断留给你。macOS 上按用户限制出网没有干净的内建做法
（`pf` 按 UID 过滤要 `pf.conf` 里写 `user` 规则，且和系统更新打架）。可选路线：

```text
pf + user 规则       /etc/pf.conf 里 `block drop out proto tcp from any to any user ccrun`
                     然后 pfctl -f /etc/pf.conf。会被系统更新覆盖，要自己管
容器 / VM            把 runtime 跑在一个网络受限的容器里。代价是项目工具链要进容器
不做                 如果这不是你的合规边界，就明确记下来"不做"，别假装做了
```

**别用 `exec_command` 的命令名黑名单来当这一层。** 一张禁止程序名的表，`env curl`、
绝对路径、一个 wrapper 脚本就绕过去了；它真正的作用是让人以为被管着。ccnm 因此**故意不做**
这张表。

---

## 6. 让 runtime 以 ccrun 身份启动

工作机 ssh 到家庭机时用哪个账号，决定了 runtime 是谁。改工作机的 `~/.ssh/config`：

```sshconfig
Host ccnm-home
    HostName <家庭机地址>
    User ccrun              # ← 从你自己的账号改成 ccrun
    IdentityFile ~/.ssh/id_ed25519
```

`ccnm-home` 这个别名后面走 Tailscale、WireGuard、公网还是局域网，ccnm 不关心也看不到
（第 6 节：ccnm owns orchestration, not networking）。

---

## 7. 再跑一次 doctor

```bash
ccnm doctor <workspace>
```

目标是这样：

```text
Runs as root            OK     ...
Runtime user            OK     ccrun
No sudo                 OK     cannot become root without a password
Not an admin            OK     not in admin, wheel or sudo
No SSH keys             OK     no readable private key in ~/.ssh
No Claude credential    OK     no Claude credentials file on this machine
No Docker socket        OK     the Docker socket is not writable by this account
exec_command            OK     the runtime account is confined
```

---

## 还没做完就想先跑？

可以，但要写出来：

```toml
[workspaces.<名字>]
allow_unconfined_exec = true
```

代价是这个 workspace 的**每一条命令结果**都会带上一句 "this runtime is NOT confined"。
接受一次风险，不等于以后就看不见它。

**别对真实项目这么干。** 一个没有 confined 的 runtime，意味着模型跑的任何命令都拥有你
自己账号的全部权限。

---

## ccnm 检查的和不检查的

| ccnm 会验 | ccnm 不会验（你自己负责） |
|---|---|
| 跑在哪个账号上 | 那个账号能读到项目以外的哪些文件 |
| 能不能免密 sudo | `/etc/sudoers.d/` 里的通配规则 |
| 在不在 admin / wheel 组 | 别的提权路径（setuid、LaunchDaemon、SSH agent 转发） |
| `~/.ssh` 里有没有可读私钥 | 别处放着的凭证 |
| 这台机器有没有 Claude 凭证 | 子进程会不会自己去访问 Anthropic |
| Docker socket 可不可写 | 别的 root-equivalent socket |
| 能不能连到 api.anthropic.com（WARN） | 你的合规边界到底是什么 |

**通过全部检查不等于 `exec_command` 可以指向不可信输入。** 它的意思只是：出事时炸的是
`ccrun` 这个账号，而不是你自己的账号。这正是设计文档要的那个区别——它值得做，但它不是 sandbox。
