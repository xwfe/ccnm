# 开发、测试、发版

从 README 搬过来的：**给要改 ccnm 的人看**，不是给用它的人看。用的人只需要
[README](../README.md)。

### 需要 Rust 1.89

`File::try_lock` —— `apply_patch` 靠它分辨"上一次提交被打断了"和"另一个提交正在跑"。
换成超时判断的话两个方向都错：有一段时间中断看不出来，而且它读时钟，NTP 一跳就会宣布
一次从没发生过的中断。

### 本地跑测试

```bash
cargo test --workspace        # 405 个测试，14 秒，不需要第二台机器，不碰网络
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

这三条就是 CI 的全部内容。测试里所有外部命令（ssh、tmux、launchctl、claude）都是注进去的
假 runner，**除了**几个故意用真东西的：`git`（list_files 的 git 模式）、`rg`（search_text）、
`/bin/sh`（进程超时和进程组那几个）。所以本机要有 `git` 和 `ripgrep`。

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

链路上**传的是什么**也能在本机验，虽然进程都是假的。`launcher.rs` 里那组测试把一端真实的
输出喂进另一端真实的输入：

```bash
cargo test -p ccnm-core --lib launcher
```

其中一个走完 `家庭机 → 工作机 → 家庭机` 一整圈——家庭机发出的启动请求，被工作机那半段真的
解开、真的握手、真的写出会话的 `mcp.json`，然后测试**读那个文件**（就是 Claude Code 会去跑的
那个 ssh，模型碰项目唯一的路），把 payload 从 argv 里解出来，比对第三跳打开的项目是不是第一跳
说的那个。两个别名、两个二进制路径故意写成四个不同的字符串，蒙对不了。

**为什么非得连起来测**：两端各自的单元测试都自己手搓消息，所以"两端各自都合理、但拿到的是
对方那个值"这类 bug 在里面永远不会出现。**加这组之前**试过：把 `start_interactive` 里的
`home_alias` 和 `work_ssh` 对调，当时那 369 个测试一个没红，而会话连到了错的机器上。现在它
红在那一条上。

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
scripts/mutate.sh        # 需要干净的工作区，跑一遍约 3 分钟
```

测试全绿只说明代码通过了测试，**不说明测试能抓住代码变错**。这个脚本一次拆掉一处 guard
（一个拒绝什么的 `if`、一个必须带的 flag、一次必须做的清理），要求每一处都让某个测试变红：

```text
RED    two files may not share a new directory
       caught by: mcp::patch::tests::two_new_files_can_share_one_new_directory
...
15 red, 0 green, 0 not applied
```

出现 `GREEN` 就是测试有洞：要么补测试，要么确认这处变异**根本不改变可观察行为**
（等价变异），说清楚然后把这条删掉。不能当成通过混过去。

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

第一次 push（远端 `github.com/xwfe/ccnm`，`origin` 已经配好）：

```bash
git push -u origin main
git push origin v0.1.0        # 这一下会触发第一个 release
```

之后发一个版本：

```bash
# 先把 Cargo.toml 里的 version 改好并提交
git tag -a v0.1.1 -m "..."
git push origin v0.1.1
```

`release.yml` 会在**版本号和 tag 对不上时直接失败**（`v0.1.1` 打在还写着 `0.1.0` 的树上，
产出的文件名就会撒谎，而这种事几个月都没人发现）。

几件要知道的：

- **只用 GitHub 官方 action**（`actions/checkout`、`actions/cache`）。第三方 action 是拿着
  token 在你仓库里跑的代码，对一个整篇都在小心"什么东西在哪台机器上跑"的项目来说，
  手写几行缓存比引入一个信任关系便宜。
- **runner 上要 `brew install ripgrep tmux`**，否则 search 那组测试会因为缺依赖而不是因为
  ccnm 有问题而失败。
- **两个 workflow 还没在 GitHub 上真跑过**（东西还没 push 上去）。但里面每一步——fmt、
  clippy、test、`scripts/dist.sh`、tag/版本校验的两个分支、release notes 的渲染——都在本机
  单独跑通了。第一次 push 之后还是要去 Actions 看一眼。
- 从浏览器下载的二进制会被 macOS 隔离，`xattr -d com.apple.quarantine ccnm` 解开；
  `curl` 下的不会。release notes 里写了这条。
