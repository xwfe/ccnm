# doctor 探 Codex 原生链（P27，2026-09-17）

范围、验收编号和"Linux 沙箱前提为什么不进 runtime-audit"的决定见 [ROADMAP P27](../plan/ROADMAP.md)；这一行怎么读见[使用说明](../usage.md#codex-原生链那一行)。本页只记实现要点、验证结果和没覆盖的东西。

**一句话结论**：`codex_exec_server = true` 且 Agent 是 Codex 时，doctor 两侧的表多出 `Codex exec-server`（中文 `Codex 原生链`）一行，做的就是 `ccnm run` 起 Codex 前的那次空会话预检；经真实 `ccnm internal exec-serve` 和假执行端离线验证了 OK、写锁被占、版本不对、没配 `codex_bin` 四种结果。没上真机，没耗模型额度。

## 一、实现

| 位置 | 改动 |
| --- | --- |
| `protocol/probe.rs` | 请求加 `codex_exec_server`（只在为真时序列化；这个结构拒绝未知字段，所以不开这条链的请求逐字节不变）；报告加 `exec_server: Option<Reported<()>>`（没跑就不序列化） |
| `work.rs` 的 `probe` | 请求要求、选中的 Agent 是 Codex、反向 hello 通过时，调 `native_runtime_preflight`——就是 `ccnm run` 用的那个函数，没有第二份实现。只看 hello、不看 MCP 握手结果，和 Runtime 安全、MCP 握手两行的条件一样 |
| `doctor.rs` | 新行 `exec_server_row`；`Subject` 带上 `codex_exec_server`（Runtime 侧取自己的配置，Agent 侧取 `runtime-resolve` 的回答）；五个固定行集合都加这一行；Runtime 侧调 `internal probe` 的超时在 90 秒上加预检自己的 90 秒上限 |
| `main.rs` / `launcher.rs` | Agent 侧 doctor 的请求带上字段；`ccnm mcp probe` 的请求明确不带 |

**行为变化（有意的）**：这一行对所有 workspace 都出现，不用这条链的是 SKIP，所以结论行的"没查"数加 1。三条已有测试的计数随之改了：`everything_good_blocks_only_on_external_or_live_session_checks` 从 2 到 3，`unreachable_work_fails_once_and_skips_the_rest` 和 CLI 的 `doctor_against_an_unreachable_agent_exits_agent_unreachable` 从 12 到 13。退出码不变：原本就有两行固定 SKIP，没有 FAIL 时一直是 `CCNM_E_NOT_READY`。

## 二、验证

环境：macOS 26.6.2 arm64，rustc 1.98.0。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | **765 passed / 0 failed**（P23 记录的是 759，本阶段新增 6 个）。`against_the_real_codex_executor_when_configured` 没设 `CCNM_TEST_CODEX_BIN`，按原样跳过 |
| `python3 scripts/check_plan.py`、`git diff --check` | 通过 |

**第一次全量跑红过 17 个，原因是磁盘满了，不是代码**：`external_mcp` 的测试报 `No space left on device (os error 28)`，当时数据卷只剩 129 MiB（同机还有别的会话在编译）。空间回到 2.6–2.9 GiB 后原样重跑，全绿。顺带看到 `$TMPDIR` 下有约 7.5 万个 `ccnm-*` 测试目录、合计约 3.4 GB，是历次测试没清掉的，与本阶段无关，没有动。

新增测试和它们各证明什么：

- `doctor::tests::the_exec_server_row_is_chosen_by_the_workspace_the_agent_and_the_preflight`：没开、Claude、OK、`CCNM_E_VERSION`、busy（`CCNM_E_POLICY`）、对端 build 不报字段、反向 SSH 失败、同机，各走到对的状态和说明；FAIL 时旁边的 MCP 握手行不受影响，退出码取这一行的码。
- `doctor::tests::the_exec_server_row_is_in_every_fixed_row_set`：Runtime 没回答、Agent SSH 失败、反向 SSH 失败三种固定集合里都有这一行。
- `doctor::tests::a_workspace_on_the_chain_asks_the_probe_for_the_preflight_and_waits_for_it`：Runtime 侧只对开了的 workspace 在请求里带字段，调用超时是 90 秒加预检上限；原有的"全绿"测试另外断言没开时请求里根本没有这个字段、超时仍是 90 秒。
- `doctor::tests::the_exec_server_row_renders_in_both_languages`：中英两种渲染，行名翻译、detail 不翻，第二行缩进在 detail 列。
- `work::tests::probe_runs_the_exec_server_preflight_only_for_codex_on_the_chain`：Claude 不跑预检（调用数不变）；Codex 跑，未绑定实例时带回与 `ccnm run` 相同的 `CCNM_E_INVALID_ARGS` 拒绝；没开时不跑。
- `exec_serve::doctor_reports_the_exec_server_chain_through_the_real_preflight`（集成）：Agent 侧是本进程里的 `work::probe` + 真实实例注册表，表由 `doctor::from_agent` 按真实二进制 `runtime-resolve` 的回答生成；Runtime 侧是真实二进制的 `hello`、`runtime-audit`、`exec-serve`，执行端是 `tests/fixtures/fake_exec_server.py`。四种结果都对；OK 那次执行端一条请求都没收到，调用顺序正好是 `hello → runtime-audit → exec-serve`；整轮结束写锁标记是 `released`。

**反向验证**：把 `probe` 里的 `selected.provider == AgentProvider::Codex` 临时改成 `!=`，上面的 `work` 单测和集成测试都红；改回后绿。

集成测试里真实渲染出来的行（英文版）：

```text
Codex exec-server       OK     empty exec-serve session on runtime: codex_bin is Codex 0.154.0, exec-server started and stopped, write guard taken and released
                               no command ran, so Codex's Linux sandbox (bubblewrap, user namespaces) is not proven here
Codex exec-server       FAIL   CCNM_E_POLICY: ccnm internal exec-serve on runtime-alias failed (exit 33): workspace write guard is busy; another session still owns this working tree
Codex exec-server       FAIL   CCNM_E_VERSION: ccnm internal exec-serve on runtime-alias failed (exit 11): Codex 0.155.0 has not been measured; this adapter requires 0.154.0
Codex exec-server       FAIL   CCNM_E_CONFIG: ccnm internal exec-serve on runtime-alias failed (exit 10): nodes.runtime.codex_bin is not set; the exec-server chain needs this Runtime to name its Codex binary
```

## 三、没覆盖的

- **ssh 那一跳是替身。**Codex Agent 的传输程序是绝对路径 `/usr/bin/ssh`，PATH 上放假 ssh 替不掉，所以集成测试用一个 runner 把"ssh 到别名后面那段命令"直接交给真实二进制，stdin、超时照原样。真实 ssh 上的这条预检由 P24 真机跑过（`ccnm run` 路径），doctor 路径没上真机。
- **集成测试不做 MCP 握手**（`mcp_calls: 0`）：握手的传输进程是直接 spawn 的，不经 runner，替不掉；它有自己的测试。
- **对端是老 build 时**：Runtime 侧的新 doctor 给一个不认识这个字段的 Agent 发 `codex_exec_server: true`，按代码推断那边会整条拒绝请求（`deny_unknown_fields`，报 `CCNM_E_VERSION`），表上是 `Agent SSH` 失败而不是这一行 SKIP。只影响开了这条链的 workspace，而这条链本来就要两端都是新 build。没有实测。
- **同时进行的其他阶段**（编号见 ROADMAP P27 开头）：握手错误码那个阶段合进来后，写锁被占时 `Remote MCP handshake` 行的码会从 `CCNM_E_RUNTIME_UNREACHABLE` 变成 `CCNM_E_POLICY`，文档里讲这一行时特意只引用了 busy 的原文，没写那一行的码。探活、MSRV 两个阶段不碰 doctor。
