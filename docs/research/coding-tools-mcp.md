# coding-tools-mcp 代码研究（Phase 1A）

问题：`lengsukq/coding-tools-mcp` 能不能给 ccnm 当 runtime contract 参考、benchmark baseline，或者直接复用代码？

结论先说：

```text
headless stdio 入口     没有。只有 HTTP（axum）transport，MCP JSON-RPC 是手写的，跑不起来就没法当 baseline
Tauri 耦合              tools/ 里只有 exec.rs / session.rs 用了 tauri::async_runtime；其余靠 ToolContext 和 harness 状态
apply_patch 可复用性    contract 值得抄（envelope 格式、staging → 同目录 temp → rename → 失败回滚），代码不值得抄
exec runtime 可复用性   contract 值得抄（argv 不走 shell、timeout、max_output_bytes、output_ref + read_output 分页），
                        实现绑在 Tauri runtime 和内存 session store 上
license                 Apache-2.0（package.json、README）；HEAD 根目录没有 LICENSE 文件，只在 old/ 下有
```

所以 Phase 1B 直接做了 ccnm 自己的最小 MCP spike，没有 vendor 任何东西。下面是依据。

## 0. 研究对象

```text
repository   https://github.com/lengsukq/coding-tools-mcp
commit       065313f81c3e534ab80f9ec1ca82f746c02f5c33（2026-08-18 23:33 +0800，"docs: refresh README feature screenshots"）
crate        src-tauri/Cargo.toml: coding-tools-mcp-desktop 0.2.1, edition 2021
license      package.json "license": "Apache-2.0"；README.md:478 "## License Apache-2.0"
             根目录没有 LICENSE / NOTICE；old/LICENSE 和 old/npm/coding-tools-mcp/LICENSE 是旧 npm 版的
clone        本机 scratchpad，只读，没有进 ccnm 仓库
```

文件位置都是 `src-tauri/src/` 下的相对路径，行号对应这个 commit。

## a. Tool inventory

`tools/registry.rs` 定义了几组：

```text
TOOL_API_VERSION = "2"                                   registry.rs:3
CORE_TOOLS       38 个                                   registry.rs:531
COMPACT_TOOLS    24 个（"Stable Tool API v2"）            registry.rs:576
```

CORE_TOOLS 全名单：

```text
server_info history_manage planning_manage task_manage
history_session_bootstrap history_session_checkpoint history_session_validate history_session_search history_session_read
planning_state create_goal update_goal create_plan update_plan request_goal_review request_plan_review
capability_health_check check_exec_environment get_default_cwd set_default_cwd list_skills get_skill
read_file list_dir list_files search_text grep_text
apply_patch exec_command write_stdin kill_session read_output
git_status git_diff git_log git_show git_blame
request_permissions view_image
```

和 ccnm 第 14 节的 7 个工具对得上的只有：`read_file`、`list_dir`/`list_files`、`search_text`（`grep_text` 是别名，
`mcp/server.rs:335-360` 的测试证明 `grep` → `grep_text` → 同一实现）、`apply_patch`、`exec_command`、`read_output`。
其余 30 个是 history / planning / task / skill / 权限 / 图片，正是设计文档第 14 节说第一版不要的东西。

initialize 的 `instructions`（`mcp/server.rs:49`）是一整段几千字节的 ChatGPT 工作流说明（history_session_bootstrap
必须先调、planning 模式由桌面端控制……），外加 skill catalog 和 history snapshot 拼接（91-95）。没有长度上限。
这是 ccnm 第 20 节"instructions bounded 8–16 KiB"的反面教材。

## b. read_file

`tools/file.rs:31-86`：

```text
参数     path, start_line?, end_line?, max_bytes?（默认 32_768，file.rs:32-34）
行选择   lines[(start_line-1)..end]，end 默认到文件尾（59-63）
截断     按字节 truncate_bytes（65），truncated=true 时给 next_start_line = 实际 end + 1（81）
返回     content, start_line, end_line, next_start_line, truncated, truncated_by, warnings["content truncated"]
```

没有 max_lines；只靠 32 KiB 字节上限兜底。ccnm 的默认 `max_lines = 200 + max_bytes = 32 KiB` 比它多一层。

路径规则在 `tools/workspace.rs`：

```text
reject_unsafe_text     168-192   拒绝以 / 或 \ 开头（177-183），拒绝任何 ".." component（187）
resolve_read_path      200-225   canonicalize 后必须 starts_with(root)（211-217）
resolve_for_write      247-327   父目录 canonicalize 后必须在 root 下（277）；目标是 symlink 且指出 root 外 → symlink_escape（320-323）
reject_write_symlink   329       写操作额外拒绝 symlink 本身
错误类型               absolute_path_denied(73) path_outside_workspace(82) symlink_escape(91)
```

这套和 ccnm 第 17 节完全一致，可以照抄 contract。

## c. search

`tools/file.rs:219-300` + 逐行流式函数 `363-`：

```text
实现          regex crate + walkdir（file.rs:7,9）。不是 rg，也没有 grep crate
参数          query, path?, regex?（默认 false，219）, case_sensitive?, max_results?（默认 1000，225-227），
              max_preview_bytes?（默认 256，229-231），max_file_bytes?（默认 2 MiB，DEFAULT_SEARCH_MAX_FILE_BYTES=14），
              context_lines?（默认 0，240-242）
早停          matches.len() >= max_results 立即返回（252, 388, 406, 421）
二进制        前 8 KiB 探测（BINARY_PEEK_BYTES=15）；文件中途出现非 UTF-8 就丢弃该文件剩余部分（386-388）
输出          每条 match 只带 preview（<=256 B）和 context 行，不回整文件
```

"只把命中结果传回"这条原则它做到了。默认 1000 条对 token 太大；ccnm 第 15 节定的 50 条 / 2 行 context / 32 KiB 更紧。
不用 rg 意味着不吃 `.gitignore`，会扫 `node_modules` / `target`；ccnm 用 rg 就没这个问题。

## d. list / glob

```text
list_dir     file.rs:98-136    max_depth 默认 1（99-101），max_entries 默认 100（104-106），truncated + warnings
list_files   file.rs:148-208   WalkDir 递归，max_results 默认 5000（149-151），每项带 type（file/symlink）和 symlink_metadata
```

不读 `.gitignore`。ccnm 定的 `max_entries = 200` 介于两者之间。

## e. apply_patch

`tools/patch.rs`。

格式（159-300）：

```text
有 "*** Begin Patch" 时按 OpenAI apply_patch envelope 解析：
   *** Add File: <path> / *** Update File: <path> / *** Delete File: <path> / *** End Patch
   hunk 用 @@ 开头，行前缀 ' ' / '+' / '-'
没有 envelope 时按普通 unified diff 解析（parse_unified_diff, 159）
Move：不支持。全文件没有 "Move to" / rename 的处理（grep move|rename 只命中 HunkLine::Remove 和 fs::remove_file）
```

定位（335-408）：

```text
find_hunk_position    从第 0 行起找第一处与 hunk 的 context+remove 行完全相等的位置（391-408）
fuzzy                 没有。"offset += 0; // reserved for future fuzzy offset"（381）
失败                  "Hunk context did not match file content."（364）
```

换行（336-347, 384-387）：原文含 `\r\n` 就整文件用 `\r\n` 输出；按 `\n` 切分后去掉行尾 `\r`；原文有结尾换行才补结尾换行。

事务（53-93 先全部在内存里算好 staged，再 `commit_staged_bytes` 426-495）：

```text
1. 每个文件先读原内容做 backup（440-447）
2. 同目录写 temp：".{name}.harness-stage-{uuid}"（452-456），写失败 → 清理 temp + restore_backups（457-461）
3. 第二轮 fs::rename(temp, path)（513-521；Windows 先 remove），删除走 fs::remove_file（483）
4. 任一步失败 → cleanup_temporary_files + restore_backups（487-491）
5. restore_backups 用 fs::write 直接覆盖（497-511），这一步本身不是原子的
```

其它：`dry_run`（17-20, 99-126）和 `patch_check`（129-137）；`.git` 等受保护资产禁止改（30-38，`is_protected_repository_asset`）；
删除"关键文件"（lock 文件、README、LICENSE、构建配置）要 `confirm=true`（39-49）；写 symlink 拒绝（62）。

**没有 stale baseline detection**：不记录/校验原文件 hash 或期望内容，只靠 hunk context 精确匹配。ccnm 第 15 节要求的
"stale baseline detection" 这里没有现成实现。

## f. exec_command

`tools/exec.rs` + `tools/session.rs` + `tools/policy.rs`。

```text
参数          cmd, cwd?, timeout_ms?（默认 30_000，exec.rs:58-60），max_output_bytes?（默认 32_768，62-64），stdin?（71），tty?
命令解析      shell_words::split → argv（138），Command::new(program).args(...)（244-262）；不经过 sh -c
内置命令      pwd / ls / dir / which 在进程内模拟，execution_mode = "native_builtin"（146-160）
cwd           必须在 workspace 内，否则 EXTERNAL_EXECUTION_NOT_ALLOWED（policy.rs:224）
policy        命令白名单 DEFAULT_ALLOWED_COMMANDS（policy.rs:19-57：pytest python npm node pnpm yarn make cargo go … git cmd powershell）
              "Shell chaining, redirection and expansion are not allowed"（239）
              DANGEROUS_COMMAND_PATTERN / INTERPRETER_MUTATION_PATTERN 正则（11-13, 242-250）
              network 只在 permission_mode = trusted | dangerous 放行（100-105）
```

输出保留（session.rs）：

```text
每个 stream 一个内存 ring buffer，上限 1 MiB（SESSION_BUFFER_BYTES=15；trim_buffer 333-340）
snapshot 只返回尾部 <= max_output_bytes（truncate_tail 345；exec.rs:281,311,320）
session 留在内存的 SessionStore 里；超时被 kill 后再保留 30 s 供 read_output（exec.rs:346-371）
read_output 按 output_ref + 每个 stream 的 byte offset 分页（registry.rs:343-345）
write_stdin / kill_session 对同一 session 生效
```

和 ccnm 第 16 节的差别：它的"完整输出"其实只是最后 1 MiB，而且在进程内存里，进程一退就没了；ccnm 要求落盘到
`~/.local/state/ccnm/runtime/<session>/`，跨 tool call 稳定。

exec 的结果契约有一份需求文档 `docs/specs/exec-contract-workspace-safety/requirements.md`：统一返回
`command, execution_mode, exit_code, stdout, stderr, duration_ms, status`，并明确"没有真实子进程隔离时不能把
workspace scope 当作已安全执行，必须 fail-closed"。这句话和 ccnm 第 18 节的判断一致。

## g. git

`tools/git.rs` 全部 shell out 到 `git`（Command::new("git")），没有 git2：

```text
git_status   max_entries 默认 500（13-15），超出 truncated（94）
git_diff     context_lines 默认 3，max_bytes 默认 65_536，超出按字节截断（103-157）
git_log      max_count 默认 20，用 --max-count=N+1 判断 truncated（166-238）
git_show     max_bytes 65_536（259-300）
git_blame    max_lines 200（351-353）
```

这是 ccnm 第 27 节"benchmark 之后再决定要不要加 git_status / git_diff"时可以直接参考的 bounded 形态。

## h. stdio / headless 入口

没有。

```text
transport          只有 HTTP：axum Router，GET /mcp（discovery）+ POST /mcp，外加 OAuth 元数据、/register、/oauth/*（mcp/listener.rs:196-224）
JSON-RPC           手写：handle_request 只认 initialize / ping / tools/list / tools/call，notifications 直接丢（mcp/server.rs:16-46）
MCP SDK            没有用 rmcp 或任何 MCP crate（Cargo.toml 里没有）
启动               由 Tauri 桌面 app 的 runtime supervisor 起监听（runtime/supervisor.rs）；main.rs / lib.rs / mcp/ / runtime/
                   里 grep stdio|headless|env::args|--serve 都没有
公网               自带 Cloudflare Tunnel / FRP（tunnel/），正是 ccnm 第 1 节禁止清单里的东西
```

所以它不能当 ccnm 的 headless stdio baseline。要拿它跑 benchmark，得把 Tauri 桌面端整个装到家庭机上，设计文档第 28 节明确不做。

## i. Tauri / desktop 耦合

`grep -c 'tauri::' tools/*.rs`：

```text
exec.rs      4    tauri::async_runtime::block_on / spawn（73, 353, 369, 383）
session.rs  13    tauri::async_runtime::JoinHandle / spawn / block_on（132, 173, 180, 373-519）
其余 13 个   0
```

直接的 Tauri 符号只有 async runtime。间接耦合更重：所有工具都通过 `ToolContext`（context.rs）拿 workspace、policy、
permission_mode、harness / planning / history 状态；`mcp/server.rs` 又依赖 `agent_context`、`usage`、`tools::history`。
`tools/` 作为独立 library 编译要先把 `ToolContext` 削成只剩 `Workspace + PolicySettings`，再把两个 async_runtime
调用换成 tokio。可做，但做完等于重写了胶水。

## j. 依赖

`src-tauri/Cargo.toml [dependencies]`：

```text
tauri 2 (tray-icon), tauri-plugin-dialog 2
serde, serde_json, uuid, thiserror, dirs
tokio (rt-multi-thread, macros, sync, net, process, io-util, fs, time)
axum 0.8, tower-http (cors), reqwest 0.12 (rustls-tls)
walkdir, regex, glob, image, base64, jsonwebtoken, sha2, shlex, shell-words, which, zip, flate2, tar, fs2
```

对比 ccnm 现在的 MCP 依赖（rmcp + tokio current_thread，78 个 crate，没有 hyper/reqwest/axum）：把它引进来会带上
axum + reqwest + tauri 整条链。

## k. 可复用模块

值得**参考 contract、自主实现**的：

```text
workspace.rs   路径规则（绝对路径拒绝、.. 拒绝、canonicalize + starts_with、symlink escape、写 symlink 拒绝）
patch.rs       envelope 格式、"先全算好再提交"、同目录 temp + rename、失败回滚；补上 Move、stale baseline、原子回滚
exec.rs        argv 不走 shell、timeout、max_output_bytes、output_ref 分页；把 retention 从内存改成落盘
git.rs         各 git 工具的 bounded 参数默认值
file.rs        read_file 的 next_start_line 语义、search 的早停
```

不值得的：history / planning / task / skill / permissions / tunnel / auth / mcp listener。

## l. license / provenance

Apache-2.0。复用代码（而不是只参考 contract）时按设计文档第 28 节：先提交 `docs/third-party/coding-tools-mcp.md`
记录 repository、commit、license、copied/derived modules、modifications，保留 attribution。因为根目录没有 LICENSE
文件，attribution 里要写清楚依据是 package.json 和 README 的声明。

目前 ccnm 没有复制它任何代码，也不打算在 Phase 2 复制：上面列的 contract 都很短，自己写比削 ToolContext 便宜。

## m. 值得记一笔的

```text
1. instructions 无上限拼接（mcp/server.rs:49-95），一个工作区的 history snapshot 也塞进去
2. search 默认 max_results = 1000，不吃 .gitignore，会扫 node_modules
3. exec 输出只留内存尾部 1 MiB，进程重启就没了；文档里叫 "retained"
4. patch 没有 stale baseline；restore_backups 用 fs::write 覆盖，回滚本身可能半写
5. 命令白名单 + 正则是它的"安全边界"，需求文档自己也承认没有子进程隔离时必须 fail-closed
   —— 和 ccnm 第 18/19 节"command parser 不是 sandbox"一致，Phase 5 的 ccrun 仍是硬门禁
```

## 结论对照第 55 节

```text
headless stdio 是否现成          否
tools runtime 与 Tauri 耦合度    直接耦合小（2 个文件的 async runtime），间接耦合大（ToolContext / harness 状态）
apply_patch 可复用性             contract 可复用；实现缺 Move / stale baseline / 原子回滚，不值得抽
exec runtime 可复用性            contract 可复用；实现的 retention 在内存、绑 Tauri runtime，不值得抽
license / provenance             Apache-2.0，根目录无 LICENSE 文件；未复制任何代码
建议                             ccnm 自己实现最小 runtime，把它当 contract 参考；benchmark baseline 用
                                 "工作机 local fixture + native Claude tools"（第 27 节的 A 组），不用它
```
