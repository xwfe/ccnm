# 开发、测试、发版

从 README 搬过来的：**给要改 ccnm 的人看**，不是给用它的人看。用的人只需要
[README](../README.md)。

后续实施先读 [计划与接续](plan/README.md)、[路线图](plan/ROADMAP.md) 和 [当前状态](plan/status.json)。当前进度以 Git 中的状态文件为准，不以聊天或本机工具缓存为准；模型工作约定见根目录 [AGENTS.md](../AGENTS.md)。

### 需要 Rust 1.89

`File::try_lock` —— `apply_patch` 靠它分辨"上一次提交被打断了"和"另一个提交正在跑"。
换成超时判断的话两个方向都错：有一段时间中断看不出来，而且它读时钟，NTP 一跳就会宣布
一次从没发生过的中断。

### 本地跑测试

```bash
cargo test --workspace        # 702 个测试，不需要第二台机器，不启动真实 Agent
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

### 改到给人看的输出时

ccnm 默认说中文（0.6.0 起）。三条规矩，违反哪条都不会编译失败，所以写在这里：

1. **翻译只在渲染出口做。** 字段存的、比较的仍然是英文。`Check.name` 是最典型的：45 处测试拿它查行，`safety_row_name` 还拿同样的字符串判断哪条 finding 是哪条——而那份报告是从另一台机器传回来的。中文只在 `Report::render_in(lang)` 里查 `row_label` 得到。**加了检查项忘了填 `row_label` 的表，只是多一行英文，不会 panic**，这是故意的。
2. **`ccnm-core` 的默认是 `Lang::En`。** 所以几百条断言渲染文本的单测继续断它们本来断的。要中文的调用点自己传 `Lang`——只有 CLI 入口决定语言，core 不存全局状态（`cargo test` 单进程多线程跑，而 `set_var` 既 `unsafe`、workspace 又 `forbid`）。
3. **列对齐用 `lang::pad`，别用 `{:<N}`。** 后者按 `char` 数补，中文一个字占两列。`Ambiguous` 类标点（`——`、`…`、`·`）按 1 列算，所以它们可以出现在句子里，但**不能进补齐列**。

哪些**绝不能**翻，以及为什么不能拿 `LANG`/`LC_ALL` 当开关，见 `crates/ccnm-core/src/lang.rs` 的模块文档。简版：MCP 面的文本给模型读、契约字符串给程序读、还有一类是 ccnm 自己要去匹配的**别人的**英文（git 的 `dubious ownership`、ssh 的 `permission denied`）——最后这类正是 locale 开关会静默破坏的东西。

集成测试统一在 helper 里设 `CCNM_LANG=en`（`crates/ccnm-cli/tests/cli.rs`），因为那些 helper 调了 `env_clear()`，CI 里 export 的传不进去。中文路径有自己的用例，其中一条断言**英文页面不含任何 CJK 字符**——加了新 doc comment 而忘了在 `zh_help` 里翻的话，是它拦下来。

Agent Provider 第一阶段的兼容回归可单独跑：

```bash
cargo test -p ccnm-core --test provider_compat
```

该测试只比对已冻结的 fixture，不更新快照；不要为了让重构通过而重新生成期望值。原始 Claude CLI 测试已移动到 `provider::claude`，项目上下文测试位于 `provider::claude::context`。

P1 的安全收紧有显式差异断言：Claude CLI/策略/旧 wire 仍对照原 golden，新增 SSH 安全选项不重录 golden；实际 MCP JSON 改走共用 Agent-side wrapper。分层合成用例见 `cargo test -p ccnm-core safety`、`cargo test -p ccnm-cli --test provider_safety` 和 `cargo test -p ccnm-cli --test mcp_read_file`；当前 OpenSSH 的 `-G -F` 验证只读临时配置，不连接网络。执行证据及限制见 [P1 记录](research/provider-safety-p1-2026-09-08.md)。

P2 用 `cargo test -p ccnm-core --test instance_config` 验证配置、双端 binding、Agent-local profiles 与只读迁移；`cargo test -p ccnm-cli --test instance_closed` 验证公共/内部入口不会把 instance 误当 legacy 执行。profile 文件、目录、auth sentinel 都是合成数据，见 [P2 记录](research/agent-instance-p2-2026-09-08.md)。

[公开协议](protocol/README.md)分两半，改哪半都要跑对应的检查。

契约那半（说明、schema、fixture）：

```bash
python3 scripts/check_protocol.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_check_protocol -q
```

校验脚本只用标准库。它检查 fixture 符合声明的 schema、错误码和说明文档的表一致、每个文档里定义的错误码都有 fixture、schema 里没有拼错的关键字。**通过不代表实现正确**，它只证明这几份文件互相自洽。

实现那半（`ccnm rpc`）：

```bash
cargo test -p ccnm-core --lib rpc::
cargo test -p ccnm-cli --test rpc
```

前者测分发器、存储和方法，执行入口用替身；后者跑真实二进制并通过管道对话，验证 stdout 只有协议、日志在 stderr、坏行不打乱流。两边都不启动 Agent，也不拨 ssh。

手动看一眼它说什么：

```bash
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"hello","params":{"client":"me","protocol_versions":["ccnm.machine/1"]}}' '{"jsonrpc":"2.0","id":2,"method":"agents.list"}' | ccnm rpc
```

黑盒契约测试（不 import 任何 ccnm 库，只走字节流）：

```bash
cargo build                                    # 测试要找 target/debug/ccnm
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest tests.test_blackbox_client -q
```

用的客户端是 [clients/python/ccnm_machine_client.py](../clients/python/ccnm_machine_client.py)——**那个文件是给外部程序抄走的**，只用标准库，复制到别的项目就能跑（有一条测试专门证明这点）。坏对端的场景（说别的协议版本、答应了握手就消失、以退出码 0 代替回答）由 [tests/fixtures/fake_rpc_peer.py](../tests/fixtures/fake_rpc_peer.py) 扮演。

找不到二进制时这组测试会 skip 而不是失败，因为 Python 测试不该依赖 cargo。看到 skip 就是没构建。

**没有真实 Agent 的验收。** 用真 provider 做双机闭环排在 P6.3。

第二阶段先保存了 [Codex 0.153.4 真机测量](research/codex-provider-probe-2026-09-07.md)，尚未开放 provider。`cargo test -p ccnm-core --test codex_measurements` 只检查 fixture，不启动模型；重放/SSH transport 脚本的 7 个离线测试另用 `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_*codex*.py' -v` 运行。反向真实交互和 tmux 生命周期的证据见 [interactive 测量](research/codex-interactive-reverse-2026-09-07.md)。

内部接线后的回归另跑 `cargo test -p ccnm-core provider::codex`；覆盖已测 JSONL、失败/拒绝/截断、私有目录权限、工具策略和版本边界。P3 公共 instance 回归另见 `cargo test -p ccnm-cli --test instance_execution`、`cargo test -p ccnm-core --test public_lifecycle`、`cargo test -p ccnm-core --test session_identity` 和 `cargo test -p ccnm-cli --test write_guard`。不要手改已有 session 的 Provider/identity；临时 Controller、历史真机与当前未复验边界见[内部接线记录](research/codex-internal-wiring-2026-09-07.md)和[支持矩阵](support-matrix.md)。

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

两个走完 `Runtime Node → Agent Node → Runtime Node` 一整圈，一个交互模式，一个 `--print`——Runtime Node发出的
启动请求，被Agent Node那半段真的解开、真的握手、真的写出会话的 `mcp.json`，然后测试**读那个文件**
（就是 Claude Code 会去跑的那个 ssh，模型碰项目唯一的路），把 payload 从 argv 里解出来，比对
第三跳打开的项目是不是第一跳说的那个。`--print` 那圈多两步：transport 里必须写着"没人在看"
（`exec_command` 跑之前会问，一个等着没人答的 print 会话会把整个超时等满），以及Agent Node真实
产出的报告再喂回Runtime Node的解码器——最后一跳不是测试自己编的文档。

两个别名、两个二进制路径故意写成四个不同的字符串，蒙对不了。Claude 启动时拿到的三样东西
（权限模式、config 目录、开场白）也全设成非默认值，因为"默认值到了"和"配置里的值到了"得
分得开——加这条之前，`start_interactive` 发默认权限模式出去，所有测试照样绿。

**真二进制这一层**（`crates/ccnm-cli/tests/cli.rs` 里 `sitting_at_*` 那两个）：往 `PATH` 最前面塞
一个假 `ssh`，它记下每次收到的 argv、按剧本作答，然后跑真的 `ccnm`。这是库测试够不着的一层：
`main.rs` 判断自己在哪台机器、clap、config 文件、以及 `--detached` 在两边各自有没有被当回事。

```bash
cargo test -p ccnm-cli --test cli sitting_at
```

坐在Runtime Node：带 `--detached` 正好一次 ssh，终端留在本地；不带，第二次 ssh 带 `-t` 把终端送过去，
第三次问会话怎么结束的。坐在Agent Node：发给Runtime Node的那一行就是人在那边会敲的命令加 `--detached`；
attach 在本地发生（是 tmux 在答，不是对面）；`ccnm result` 也在本地答（读的是这台自己写的
session 目录）；config 里写的Runtime Node ccnm 路径是真被跑的那个。能这么做是因为 ccnm 自己调 ssh
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

**这组抓出来的两个**：①Agent Node上 `ccnm xshun "开场白"` 会把开场白悄悄丢掉——Runtime Node那半边把它
带到底，对Agent Node提同样的要求时才发现那边根本没东西带它（现在走 stdin 送过去，见
[使用说明](usage.md#prompt)）。②`ccnm result` 在Agent Node上会答 "workspace 未定义"，
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
bash scripts/deploy.sh <另一台的 ssh 别名> [workspace]
```

在有 Rust toolchain 的那台上跑（通常是Agent Node，Runtime Node常常没装 cargo）。它编译、按
[运维的「安装与升级」](operations.md#安装与升级)那个安全办法装到两边、重启 controller（哪台有它就重启哪台）、然后跑一次
`ccnm doctor`。正在跑的会话不受影响。

最后那次 `doctor` 是**先在本机跑、只在收到 `CCNM_E_CONFIG`(10) 时才转去另一台**。别改成
"有 config 文件就在这台跑"：Agent Node也有 config，里面只有回家的路、一个 workspace 都没有，
于是那个判断在唯一答不出来的机器上说"是"。

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

一个版本出**两个下载**，因为 ccnm 的两半跑在不同的地方。

在 macOS 上：

```bash
bash scripts/dist.sh
```

产出 `dist/ccnm-<version>-macos-universal.tar.gz`（+ `.sha256`）。是 arm64 + x86_64 的通用
二进制：16.9 MB 二进制，打包后 6.1 MB。做成通用的原因是两台机器可能一台 M 系列一台 Intel，
让人自己挑架构下载迟早出事。

在 x86_64 的 Linux 上：

```bash
bash scripts/dist-linux.sh
```

产出 `dist/ccnm-<version>-linux-x86_64.tar.gz`（+ `.sha256`）。**这一个只是 Runtime 那一半**
（`internal mcp-serve` 和七个工具）。Agent 那一半是 launchd LaunchAgent，在 Linux 上根本不跑；
发这个包不等于说它跑。

它是**本机构建，不交叉编译**：链接别人家的 glibc 要别人家的工具链，而一个本机跑不起来的
二进制就是没人验过的二进制——release 里那一步 `ccnm --version` 正是在验它。

它还会把 **glibc 下限从二进制里量出来**写进 `dist/glibc-floor.txt`（`objdump -T` 里最高的那个
`GLIBC_x.y` 符号版本），release notes 引用的就是这个数。不量而是照着构建机的发行版猜，用户会在
一台"看起来支持"的机器上撞到 `version GLIBC_2.39 not found`。

两个脚本的版本号都取自二进制自己（`ccnm --version`），不是从 Cargo.toml 抄的——文件名不可能跟
里面的东西不一致。

**tar 保留执行位**，解出来就是 `rwxr-xr-x`，不像 `scp`（那个坑见上面 `permission denied`
那一节）。所以走 release 下载装的人不需要再 `chmod +x`。

**这两个脚本在 CI 里都写成 `bash scripts/…`**，不是直接 `scripts/…`：它们的执行位不保证在
（`dist.sh` 就在一次整理提交里被去掉过）。一个因为 `Permission denied` 失败的 release，是在 tag
已经推出去、撤不回来之后才失败的。

### GitHub 上的自动构建和发版

`.github/workflows/` 里两个：

```text
ci.yml       每次 push / PR：
             test           (macos-latest)  fmt + clippy + 全部测试 + 跑一下二进制
             linux-runtime  (ubuntu-24.04)  clippy + 全部测试 + 构建 Linux 发布物
release.yml  推 tag（v*）：
             macos    门禁 → dist.sh       → 校验 tag 和版本号一致 → 上传
             linux    门禁 → dist-linux.sh → 校验 tag 和版本号一致 → 上传
             publish  两个都绿之后，用两份产物建一个 release
```

**Linux job 那一栏绿了，意思是"代码在 Linux 上编得过、测试过得去"，不是"Agent 那一半支持
Linux"。** 别因为这个 job 绿了就去改支持矩阵。它存在的理由很具体：P12 在真实 Debian 13 上第一次
跑门禁就红了两条，其中一条是真缺陷（超时只杀进程组的 leader，因为 dash 会 fork 而 bash 会 exec），
在 macOS 上五次五绿、在 Linux 上五次五红。没有这个 job，下一条这样的东西还是要等到有人去真机上
跑才发现。

**runner 上要 `ubuntu-24.04`，不是 `ubuntu-latest`**：构建机的 glibc 就是下载物的运行下限，这个
数不能因为 GitHub 把标签滚到下一个 LTS 就悄悄变了。CI 和 release 用同一个镜像，否则门禁跑的地方
和产物出的地方不是一台机器，门禁就管不着产物。

**macOS 那两条真跑过（2026-09-05 第一次 push）**：`main` 上的 ci 绿，`v0.1.0` 和 `v0.2.0` 各触发
一次 release，都绿，两个 release 建出来了、带 tar 和 sha256。下载回来验过：sha256 对得上、
`lipo -info` 是 `x86_64 arm64`、解出来 `rwxr-xr-x`、`ccnm --version` 报的号跟 tag 一致。

**`ci.yml` 的 Linux job 也真跑过了，而且它第一次跑就抓到一个真缺陷**：`kill -KILL -<pgid>`
在 Linux 上从来没杀成过进程组，还报成功（见[支持矩阵](support-matrix.md)那一段）。修掉之后
clippy 干净、681 passed / 0 failed（当时的数字）、`scripts/dist-linux.sh` 在 runner 上产出了包。

**`release.yml` 的 Linux job 还没在 runner 上跑过**——它只在推 tag 时触发，第一次运行就是
第一次发版。它调用的东西（同一套门禁、同一个打包脚本）已经在 `ci.yml` 的 Linux job 和一台真实
Debian 13 上各验过一遍。

**推 tag 就是发版，撤不回来**——GitHub release 建出来了，别人可能已经下过。所以推之前先把门禁
和打包在本机跑一遍：macOS 上 `bash scripts/dist.sh`，Linux 那半要么找一台 x86_64 的 Linux 跑
`bash scripts/dist-linux.sh`，要么接受"第一次在 runner 上跑"这个风险——它失败的时候 tag 已经推
出去了。

发一个版本：

```bash
# 先把 Cargo.toml 里的 version 改好并提交
git tag -a v0.2.1 -m "..."
git push origin v0.2.1
```

`release.yml` 会在**版本号和 tag 对不上时直接失败**（`v0.2.1` 打在还写着 `0.2.0` 的树上，
产出的文件名就会撒谎，而这种事几个月都没人发现）。

几件要知道的：

- **只用 GitHub 官方 action**（`actions/checkout`、`actions/cache`、`actions/upload-artifact`、
  `actions/download-artifact`——后两个是给两个平台的产物在 job 之间传递用的）。第三方 action 是拿着
  token 在你仓库里跑的代码，对一个整篇都在小心"什么东西在哪台机器上跑"的项目来说，
  手写几行缓存比引入一个信任关系便宜。
- **runner 上要装 ripgrep 和 tmux**（macOS 用 `brew install`，Linux 用
  `sudo apt-get install -y`），否则 search 那组测试会因为缺依赖而不是因为 ccnm 有问题而失败。
- **runner 编出来的二进制比本机的大一点**（18.4 MB vs 16.9 MB，打包后 6.2 vs 6.1 MB）。
  toolchain 版本不同而已，不是哪边出了问题；也因此**两边的 sha256 对不上是正常的**，
  校验和只用来验"下载到的那个文件没坏"，不是用来比对本机构建的。
- 从浏览器下载的二进制会被 macOS 隔离，`xattr -d com.apple.quarantine ccnm` 解开；
  `curl` 下的不会。release notes 里写了这条。
