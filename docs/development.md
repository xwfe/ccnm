# 开发、测试、发版

从 README 搬过来的：**给要改 ccnm 的人看**，不是给用它的人看。用的人只需要
[README](../README.md)。

### 需要 Rust 1.89

`File::try_lock` —— `apply_patch` 靠它分辨"上一次提交被打断了"和"另一个提交正在跑"。
换成超时判断的话两个方向都错：有一段时间中断看不出来，而且它读时钟，NTP 一跳就会宣布
一次从没发生过的中断。

### 本地跑测试

```bash
cargo test --workspace        # 411 个测试，15 秒，不需要第二台机器，不碰网络
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

这三条就是 CI 的全部内容。测试里所有外部命令（ssh、tmux、launchctl、claude）都是注进去的
假 runner，**除了**几个故意用真东西的：`git`（list_files 的 git 模式）、`rg`（search_text）、
`/bin/sh`（进程超时和进程组那几个）。所以本机要有 `git` 和 `ripgrep`。

**`cargo test` 是 fail-fast 的**：第一个失败的测试二进制之后就不跑了，而 cli 集成测试排在
core lib 前面。看到 cli 红了一条，别以为 lib 那 379 个是绿的——它们根本没跑。要全跑
`--no-fail-fast`。

写新测试时两条硬规矩，都是撞出来的：

- **临时目录必须带 `std::process::id()`。** 同一个用户的两个 `cargo test` 进程共用一个
  `$TMPDIR`（一边跑变异测试一边开发就是这个局面，CI 上一台 runner 跑两个 job 也是），
  路径撞上就是互删对方的文件。表现极具迷惑性：`patch` 的 11 个 journal 测试原来把目录写成
  `root.join("../xxx-state")`，`root` 带 pid 而 `..` 正好走出去，于是**一个关于文件锁的测试
  偶发失败**，单跑 25 次不复现。两个进程并发跑，三次全挂。
- **别写"多久之内跑完"这种断言，除非余量是数量级的。** `supervise` 那条原来要求 5 秒内完成
  （证明没有干等一个没关的 stdin），机器一忙就红。现在会话超时 60 秒、断言 10 秒——真卡住是
  60 秒，跟 10 秒差 6 倍，忙不忙都分得开。

### 不用第二台机器，能测到哪一步

MCP runtime 那一半可以完全在本机验，它跟网络无关：

```bash
ccnm mcp probe <workspace> --local --calls 100
```

它把 `ccnm internal mcp-serve` 当子进程起来，走真的 MCP 协议（initialize、tools/list、
100 次 workspace_info），最后证明**是同一个进程答完了全部**——单进程、单会话，
不是每次调用起一个。输出是真实延迟：

```text
initialize in 113 ms, tools/list (7 tools, 8236 B), instructions 453 B (...),
workspace_info x100 p50 0 ms p95 0 ms max 0 ms, pid 44296 throughout
```

（毫秒那一栏在本机全是 0 —— 后面跟着的 JSON 里有微秒：`call_p50_us: 65`、
`call_p95_us: 89`、`call_max_us: 189`。走 ssh 的时候这些数变成 20–30 毫秒，
差的那部分就是链路。）

跟走 ssh 的那次（`ccnm mcp probe <ws>`，不带 `--local`）一比，差值就是链路成本。

链路上**传的是什么**也能在本机验，虽然进程都是假的。分两层。

**库这一层**（`launcher.rs` 里那组）把一端真实的输出喂进另一端真实的输入：

```bash
cargo test -p ccnm-core --lib launcher
```

两个走完 `家庭机 → 工作机 → 家庭机` 一整圈，一个交互模式，一个 `--print`——家庭机发出的
启动请求，被工作机那半段真的解开、真的握手、真的写出会话的 `mcp.json`，然后测试**读那个文件**
（就是 Claude Code 会去跑的那个 ssh，模型碰项目唯一的路），把 payload 从 argv 里解出来，比对
第三跳打开的项目是不是第一跳说的那个。`--print` 那圈多两步：transport 里必须写着"没人在看"
（`exec_command` 跑之前会问，一个等着没人答的 print 会话会把整个超时等满），以及工作机真实
产出的报告再喂回家庭机的解码器——最后一跳不是测试自己编的文档。

两个别名、两个二进制路径故意写成四个不同的字符串，蒙对不了。Claude 启动时拿到的三样东西
（权限模式、config 目录、开场白）也全设成非默认值，因为"默认值到了"和"配置里的值到了"得
分得开——加这条之前，`start_interactive` 发默认权限模式出去，所有测试照样绿。

**真二进制这一层**（`crates/ccnm-cli/tests/cli.rs` 里 `sitting_at_*` 那两个）：往 `PATH` 最前面塞
一个假 `ssh`，它记下每次收到的 argv、按剧本作答，然后跑真的 `ccnm`。这是库测试够不着的一层：
`main.rs` 判断自己在哪台机器、clap、config 文件、以及 `--detached` 在两边各自有没有被当回事。

```bash
cargo test -p ccnm-cli --test cli sitting_at
```

坐在家庭机：带 `--detached` 正好一次 ssh，终端留在本地；不带，第二次 ssh 带 `-t` 把终端送过去，
第三次问会话怎么结束的。坐在工作机：发给家庭机的那一行就是人在那边会敲的命令加 `--detached`；
attach 在本地发生（是 tmux 在答，不是对面）；`ccnm result` 也在本地答（读的是这台自己写的
session 目录）；config 里写的家庭机 ccnm 路径是真被跑的那个。能这么做是因为 ccnm 自己调 ssh
是按名字找的——只有 `mcp.json` 里给 Claude 的那行 transport 写的是绝对路径。

开场白那条单拎出来说，因为它是唯一一个**不在 argv 里**的跨机器值：假 ssh 除了记 argv，还在
命令行里出现 `--prompt-stdin` 时把 stdin `cat` 到另一个文件。测试要的是三件事同时成立——
远端那行以 `--prompt-stdin` 结尾、字节一个不差地出现在 stdin 那个文件里、argv 里**一个字都
没有**。用的句子带引号带撇号带换行，就是为了让"不小心塞进命令行"这条路走不通。（假 ssh 只在
看见那个 flag 时才 `cat`：attach 那一跳的 stdin 是测试进程自己的，读到不了 EOF，无条件 `cat`
会把整个测试挂住。）

**为什么非得连起来测**：两端各自的单元测试都自己手搓消息，所以"两端各自都合理、但拿到的是
对方那个值"这类 bug 在里面永远不会出现。**加这组之前**试过：把 `start_interactive` 里的
`home_alias` 和 `work_ssh` 对调，当时那 369 个测试一个没红，而会话连到了错的机器上。现在它
红在那一条上。

**这组抓出来的两个**：①工作机上 `ccnm xshun "开场白"` 会把开场白悄悄丢掉——家庭机那半边把它
带到底，对工作机提同样的要求时才发现那边根本没东西带它（现在走 stdin 送过去，见
[README](../README.md#你坐在哪台前面都行)）。②`ccnm result` 在工作机上会答 "workspace 未定义"，
而那台机器上就躺着那个 session 的全部输出。两个都是"这半边根本没实现"，而不是实现错了——
只有把另一半的命令逐条对着提一遍才看得见。

**还是测不到的**：controller / 登录会话是不是真的能读到 Keychain、tmux 里 Claude 到底起没起来、
真实的延迟——那些需要两台机器（或者一台机器 ssh 自己，见下）。

### 单机环回（一台 Mac 也能跑全链路）

把两个角色都指向 `localhost`：打开「系统设置 → 通用 → 共享 → 远程登录」，把自己的公钥加进
`~/.ssh/authorized_keys`，然后 config 里两个 host 都写 `localhost`。

**这条路我没在这台机器上验过**——它要往你的 `~/.ssh/authorized_keys` 里加东西，那是你的机器，
我不动。机制上没有理由不通（ccnm 对两端唯一的要求就是 ssh 别名能通），但我没跑过就不说它跑通了。

### 两台机器的开发循环

```bash
scripts/deploy.sh <另一台的 ssh 别名> [workspace]
```

在有 Rust toolchain 的那台上跑（通常是工作机，家庭机常常没装 cargo）。它编译、按
[README 的「升级」](../README.md#升级)那个安全办法装到两边、重启 controller（哪台有它就重启哪台）、然后跑一次
`ccnm doctor`。正在跑的会话不受影响。

### 变异测试

```bash
scripts/mutate.sh        # 需要干净的工作区，38 个 case
```

测试全绿只说明代码通过了测试，**不说明测试能抓住代码变错**。这个脚本一次拆掉一处 guard
（一个拒绝什么的 `if`、一个必须带的 flag、一次必须做的清理），要求每一处都让某个测试变红：

```text
RED    two files may not share a new directory
       caught by: mcp::patch::tests::two_new_files_can_share_one_new_directory
...
38 red, 0 green, 0 not applied
```

出现 `GREEN` 就是测试有洞：要么补测试，要么确认这处变异**根本不改变可观察行为**
（等价变异），说清楚然后把这条删掉。不能当成通过混过去。

出现 `COULD NOT APPLY` 是 case 过时了：它是照着当时的源码写的，源码一动它就贴不上，什么也
证明不了。改写或者删掉，别留着。（`sweep_stale_temps` 多了个参数之后那条就是这样过时的。）

**别在中间打断它。** 每个 case 是"改源码 → `cargo test` → `git checkout` 还原"，停在中间
变异就留在工作区里——看着像干净的树，实际少了一个 guard，`git status` 会显示一个你没改过的
文件是 `M`。脚本退出时（包括 Ctrl-C）会把它可能碰过的文件全还原一遍，但 `kill -9` 拦不住，
所以中断后 `git status` 看一眼，有 `M` 就 `git checkout` 它。要一边跑一边接着干活，放到一个
`git worktree` 里跑：它的还原只碰自己那份。

**耗时差 5 倍，按 target 热不热算。** 主树上 `target/` 是热的，一个 case 只重编改动的那个
crate，38 个大约 15 分钟；新开的 `git worktree` 里 `target/` 是空的，第一次全量编译加上
每次重编，同样 38 个跑了约 50 分钟。想边跑边干活就得用 worktree，那就按后面这个数等。

### 打包

```bash
scripts/dist.sh
```

产出 `dist/ccnm-<version>-macos-universal.tar.gz`（+ `.sha256`）。是 arm64 + x86_64 的通用
二进制：16.9 MB 二进制，打包后 6.1 MB。做成通用的原因是两台机器可能一台 M 系列一台 Intel，
让人自己挑架构下载迟早出事。

版本号取自二进制自己（`ccnm --version`），不是从 Cargo.toml 抄的——文件名不可能跟里面的东西不一致。

**tar 保留执行位**，解出来就是 `rwxr-xr-x`，不像 `scp`（那个坑见上面 `permission denied`
那一节）。所以走 release 下载装的人不需要再 `chmod +x`。

### GitHub 上的自动构建和发版

`.github/workflows/` 里两个：

```text
ci.yml       每次 push / PR：fmt + clippy + 全部测试 + 跑一下二进制
release.yml  推 tag（v*）：过一遍同样的门禁 → dist.sh → 校验 tag 和版本号一致 → 建 release
```

**东西还在本地**：远端 `github.com/xwfe/ccnm` 上只有最早那个加 LICENSE 的 commit，
`origin` 配好了但从没推过，`v0.1.0` 和 `v0.2.0` 两个 tag 也都只在这台机器上。所以这两个
workflow 一次都没跑过。

第一次 push：

```bash
git push -u origin main
git push origin v0.1.0 v0.2.0   # 每个 tag 触发一次 release
```

**推 tag 就是发版，撤不回来**——GitHub release 建出来了，别人可能已经下过。所以推之前
本机先把门禁和 `scripts/dist.sh` 跑一遍。

之后发一个版本：

```bash
# 先把 Cargo.toml 里的 version 改好并提交
git tag -a v0.2.1 -m "..."
git push origin v0.2.1
```

`release.yml` 会在**版本号和 tag 对不上时直接失败**（`v0.2.1` 打在还写着 `0.2.0` 的树上，
产出的文件名就会撒谎，而这种事几个月都没人发现）。

几件要知道的：

- **只用 GitHub 官方 action**（`actions/checkout`、`actions/cache`）。第三方 action 是拿着
  token 在你仓库里跑的代码，对一个整篇都在小心"什么东西在哪台机器上跑"的项目来说，
  手写几行缓存比引入一个信任关系便宜。
- **runner 上要 `brew install ripgrep tmux`**，否则 search 那组测试会因为缺依赖而不是因为
  ccnm 有问题而失败。
- **没跑过不等于跑得起来。** 里面每一步——fmt、clippy、test、`scripts/dist.sh`、tag/版本
  校验的两个分支、release notes 的渲染——都在这台机器上单独跑通了，但 runner 是一台干净的
  macOS，`rg` 和 `tmux` 靠上一条那句 `brew install` 才有。第一次 push 之后去 Actions 看一眼
  再说话。
- 从浏览器下载的二进制会被 macOS 隔离，`xattr -d com.apple.quarantine ccnm` 解开；
  `curl` 下的不会。release notes 里写了这条。
