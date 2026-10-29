# P117：Claude Code 后台 agent 拿只读工具，不再动主会话的记录（2026-10-30）

P116.4 查明：10-23、10-24 两次"MCP 没连上"，是 Claude Code 的后台 agent 另开了一条 ccnm 通道，撞上了单写锁。用户 10-29 听了建议，定给它只读工具、不关掉后台 agent（关掉的开关没量过，用户也在用这个功能），10-30 说继续。这份记录写：量到了什么、怎么认出后台 agent、改了什么、验到哪一步。

## 1. 量到的事实（Claude Code 2.1.312，零额度）

都在 AI 机器上量，不联网：模型地址指到一个没人监听的端口，或者指到本机的假模型。

- **后台 agent 是另一个 `claude` 进程，启动参数照抄父会话的。** 二进制里的拼参数函数（`Wpe`）带上父会话的 `--settings`、`--managed-settings`、`--plugin-dir`、`--add-dir`、`--mcp-config`、`--setting-sources`、`--strict-mcp-config`、`--restricted`；守护进程拉起时环境里加 `CLAUDE_CODE_SESSION_KIND=bg`。后台 agent 能不能开，由服务端下发的开关（`isAgentsFleetEnabled`）决定，所以用假 key 起不出一个真的后台 agent。
- 所以它**加载同一份 `mcp.json`**：ccnm 的通道从里面的会话目录读出会话 id，后台 agent 连上来带的就是父会话的 ccnm 会话 id，撞写锁被拒。
- 它也**加载同一份 `settings.json`，跑 ccnm 的会话钩子**：`SessionStart` 把它自己的对话 id 记进主会话的 `conversations.json`；`Stop` 按它的最后一句改写主会话的 `handoff.md`，那句里没有交接就删掉。这一条以前没发现，它影响 `-c` 接哪段对话、P115 换机器时带什么，还会弄丢主会话写好的交接。
- **环境变量认不出它。** 让 Claude Code 带着 `CLAUDE_CODE_SESSION_KIND=bg` 和一个对照变量起，MCP 子进程记下自己收到的环境：对照变量、`ANTHROPIC_*`、`CLAUDE_CODE_SESSION_ID` 都在，唯独 `CLAUDE_CODE_SESSION_KIND` 被去掉了。
- **对话 id 也认不出它。** 它的对话 id 和主会话不同，可它的 `SessionStart` 钩子会把这个 id 写进同一份记录，所以拿记录比对没有用。
- **进程关系认得出。** MCP 子进程和钩子命令都记下自己的祖先链：两者的父进程都直接是 `claude`（钩子经 `sh -c` 时，sh 直接 exec 了）。主会话的 `claude` 就是监督进程记在 `agent.pid` 里的那个；后台 agent 的父进程是它自己的 `claude`。P116 判断"通道通不通"，靠的也是"通道是 `agent.pid` 的子进程"。

## 2. 改了什么

- **AI 机器**（`session::from_own_agent`）：Claude 会话的 `internal agent-transport` 和 `internal session-hook` 先看自己的父进程或祖父进程是不是 `agent.pid`。
  - 是：照旧。
  - 不是：通道在载荷里写 `policy: "read"`；钩子什么都不做，也不输出。
  - 判不出来时（没有 `agent.pid`、不是 Unix）当作主会话，也就是以前的行为。判错只会错成"当成主会话"，那就是今天的行为：撞锁被拒。
  - Codex 会话不问：没有后台 agent，它的 MCP 子进程是谁起的也没量过。
- **协议加法**：Agent 会话的 mcp-serve 载荷（legacy 的 `ServePayload`、实例的 `OpenPayload`）的 `policy` 多认一个 `read`，在进程里是 `Entry::Companion`。
  - 它不写（`writes()` 为假），所以不拿写锁，工具和外部 MCP 的 `read` 模式一样。
  - `initialize` 的 instructions 换成一段说明：这条连接不是会话自己的 Claude 开的、为什么只读、要改的东西回主会话去改；项目的 `CLAUDE.md` 只点名不内联，免得说明被截掉。
  - 硬要调写工具时，报错说同样的原因。
  - legacy 的 `serve` 以前不看 `policy`，现在只认 `coding` 和 `read`，别的值报 `CCNM_E_POLICY`。
  - 会话自己的通道发的载荷一个字没变。旧 Runtime 不认 `read`（实例写法）或不看它（legacy，于是以 coding 去拿锁），两种都是照旧被拒。
- 写锁被占时的报错最后两行改了措辞：原来说后台 agent"拿不到工具"，现在说新版本会给它只读工具。协议样本 `start-refused-busy.json` 同步改了。

## 3. 离线验证

- 先红：钩子那条（`a_background_agents_hooks_leave_the_session_alone`）只用旧接口。把新加的判断拿掉再跑，它红了：后台 agent 的 `SessionStart` 把本该给主会话的交接（`carried.json`）拿走了，第一处断言就失败。其余新测试用到新函数，在旧代码上编译不过。
- 单元测试 7 条：
  - 父/祖父进程判定的纯函数（6 种情形）；
  - 拿本测试进程的真实祖先链跑一遍：父进程、祖父进程算主会话，自己不算；
  - 两种载荷在"不是本会话的 claude"时都是 `read`，`command()` 照旧是 `coding`；
  - 钩子：后台 agent 的启动、结束事件不改对话记录、不碰交接和待带的东西，主会话的照常记；
  - `policy` 到入口的映射，只认两个值；
  - Runtime 以 `read` 打开实例会话时是 `Companion`、不写、照样带核验过的绑定；
  - `Companion` 的工具表和外部 `read` 一样，instructions 和报错说明原因，并且放得进 Claude Code 的上限。

## 4. 零额度真机

和 [P114、P115 的模拟](2026-10-30-p114-p115-sim.md)同一套搭法，只用一份部署 a，两边都用带 P117 的发布构建（sha256 `954bc5b2…`，只放在临时目录），Claude 接 AI 机器本机的假模型。

1. 主会话照常起：第一条请求里有 12 个 ccnm 工具；`apply_patch` 把文件写进项目机器的项目目录；回一段交接，`handoff.md` 存下。项目机器上写锁归它。
2. 从 ssh 的 shell 另起一个 `claude` 扮后台 agent。它不是主会话的子进程，这一点和守护进程一样；参数照抄主会话的 `--settings`、`--mcp-config`、`--strict-mcp-config`、`--setting-sources`、`--permission-mode`，换一个新的对话 id，环境里加 `CLAUDE_CODE_SESSION_KIND=bg`，用 `-p` 跑一句。
   - 它连上了（MCP 日志 `Successfully connected … in 633ms`）。它开着工具搜索，ccnm 的工具在延迟列表里，正好是只读的 7 个：`workspace_info`、`read_file`、`list_files`、`search_text`、`load_skill`、`view_image`、`read_notebook`。
   - 假模型让它工具搜索 `read_file` 和 `apply_patch`，只回来 `read_file`；接着 `read_file README.md` 读到了项目机器上的内容。
   - 它的请求里有那段"This connection was not opened by the Claude Code process ccnm started for this session …"的说明。
3. 两次后台运行前后，主会话的 `conversations.json`、`handoff.md` 的 sha1 完全一样。按旧代码，后台 agent 第一次那句没有交接的回复会把 `handoff.md` 删掉。项目机器上的写锁一直是主会话的。
4. 后台跑完，把主会话写的文件删掉，再让主会话写一次，写回来了。

收尾：停会话、卸掉模拟 Controller、停假模型；两边的临时目录、环境探针目录、模拟 claude 在日用缓存里留下的 MCP 日志目录都删了。日用 `~/.claude.json` 里没有留下这次的记录，日用 Controller 的 pid 前后一样。

## 5. 没验的

- **真的后台 agent**：开关由服务端下发，假 key 起不出来。真机上用的是照它的启动参数另起的 `claude`。"后台 agent 的父进程不是主会话的 `claude`"这一条，是从它由守护进程另起推出来的，没在真的后台 agent 上量过祖先链。如果哪个版本改成由主会话直接起后台 agent，判定会把它当成主会话，结果就是今天的行为（撞锁被拒），主会话不受影响。
- 后台 agent 在交互模式下的界面没看过。
- Codex 没改。
- Linux AI 机器上没跑：父进程关系应该一样（同一个 Bun 运行时），没量。

## 6. 门禁

见提交时 [status.json](../plan/status.json) 里 P117 的证据。
