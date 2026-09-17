# P38 `search_text` 的 glob 不再越过 `.gitignore`（2026-09-17）

环境：macOS arm64 开发机；ripgrep 15.2.0。零额度，没上真机。

## 1. 结论

- **缺陷**：给了 `glob`，`search_text` 就会搜 `.gitignore` 排除的东西。不只是 `**` 这种能匹配目录的 glob——只能匹配文件的 `*.log`、`**/*.yml` 也会搜到被忽略的 `run.log`、`secret.yml`。
- **P37 交接时建议的修法（拒绝能匹配目录的 glob）不成立**，理由就是上一句。
- **实际修法**：调用方的 glob 不再作为 rg 的 `--glob`。文件名部分用 `--type-add` 交给 rg 缩小范围，整条 glob 由 ccnm 按 rg 原来的规则过滤结果。唯一的结果变化是被忽略的文件不再出现；glob 的含义不变，同一组回归测试在旧实现和新实现上都通过。
- 顺带：`type` 和 `glob` 同时给时两者都要满足，不再拒绝（P37 拒绝的理由随 `--glob` 一起消失了）。

## 2. P38.1 实测

草稿工作区：`.git/`、`.gitignore` 写 `target/`、`secret.yml`、`*.log`；文件 `src/a.rs`、`src/nested/b.rs`、`a/src/x.rs`、`target/t.rs`、`secret.yml`、`app.yml`、`run.log`、`src/.hid.rs`。每条命令都带 `--no-config --no-hidden -g '!.*' -g '!.git'`，查询词 `foo`，`-l` 列文件。

**踩过的坑**：第一轮在 zsh 里把这几个排除参数放进一个变量再展开，zsh 默认不按空格拆分，rg 收到的是一个参数，结果全是空的。改用 bash 数组重跑，下面是第二轮的结果。

| 参数 | 列出的文件 | 说明 |
| --- | --- | --- |
| 不给 glob | `a/src/x.rs` `app.yml` `src/a.rs` `src/nested/b.rs` | 基线：被忽略的都不在 |
| `-g '**/*.yml'` | `app.yml` **`secret.yml`** | 只能匹配文件的 glob 也越过 `.gitignore` |
| `-g '*.log'` | **`run.log`** | 同上 |
| `--type-add 'ccnmglob:*.yml' --type ccnmglob` | `app.yml` | 类型遵守 `.gitignore` |
| `--type-add 'ccnmglob:*'`（或 `**`） | 与基线相同 | `*`、`**` 当类型 glob 等于不过滤 |
| `--type-add 'ccnmglob:*.rs'` | 不含 `src/.hid.rs` | 类型越过 `--no-hidden` 的问题由 `-g '!.*'` 挡住 |
| `-g 'src/*.rs'` | `src/a.rs` | 含 `/` 从根锚定，`*` 不跨目录 |
| 同上，但搜索路径是 `src` | `src/a.rs` | 锚点仍是 cwd（workspace 根），不是搜索路径 |
| `-g '*.rs'`，搜索路径是 `src` | `src/a.rs` `src/nested/b.rs` | 不含 `/` 比文件名、任意深度 |
| `-g 'src/**'` | `src/a.rs` `src/nested/b.rs` | |
| `-g '**/src/*.rs'` | `a/src/x.rs` `src/a.rs` | |
| `-g '{*.rs,src/*.py}'`（另加 `a.rs`、`src/b.py`、`src/c.rs`） | `a.rs` `src/b.py` | 整条含 `/`，所以 `*.rs` 也从根锚定 |
| `-g 'src/'` | 无 | 以 `/` 结尾只匹配目录 |
| `-g './src/*.rs'` | 无 | rg 不认 `./` 前缀；ccnm 的 `Glob` 会去掉它，这一处结果变多 |
| `-g '!*.rs'` | `app.yml` | 排除规则不越过 `.gitignore` |
| `--type-add 'ccnmglob:include:rust'` | 所有 Rust 文件 | 被当成"包含 rust 类型"的指令 |
| `--type-add 'ccnmglob:x:y.txt'` | 报错 `invalid definition` | 文件名部分含 `:` 不能做类型 |

原因（rg 用的 `ignore` crate）：一个路径依次过 override（`--glob`）→ `.gitignore` 等忽略文件 → 类型 → 隐藏文件；override 只要命中（放行或排除）就立刻定案。所以放行型 glob 一命中，后面三关都跳过；排除型 glob 只会拦东西。类型只作用于文件，而且排在忽略文件之后。

## 3. 实现

- `Glob` 记下两件事：去掉结尾的 `/` 之后还含不含 `/`（`rooted`）、是不是以 `/` 结尾（`dir_only`）。`matches_file_as_rg` 按第 2 节的规则匹配文件路径；`file_names` 给出每个备选的最后一段。
- `search_text`：
  - 放行型 glob：每个不同的文件名部分加一条 `--type-add=ccnmglob:<名字>`，再加 `--type=ccnmglob`。有任何一个名字含 `:`、或者调用方自己给了 `type`（rg 对多个类型取并集），就不缩小，只靠过滤。
  - 每条 `match` / `context` 事件的路径先过越界检查，再过 `matches_file_as_rg`，不匹配的直接丢掉。
  - 以 `!` 开头的 glob 照旧作为 `--glob` 交给 rg。
- 提交 462686b。

## 4. 验证

- 新测试 `a_glob_does_not_reach_what_gitignore_rules_out`（`*`、`**`、`**/*`、`*.rs`、`**/*.rs`、`src/**`、`{src,ignored}/**` 七种 glob）和 `type_and_glob_together_mean_both`：把 `search.rs` 的实现部分换回 P38 之前、测试部分保持新版，这两条失败；新实现下通过。
- `a_glob_still_means_what_it_meant_to_ripgrep`（`*.rs` 任意深度、`src/*.rs` 不进子目录、`**/src/*.rs`、`{*.rs,src/*.py}`、`src/` 匹配不到文件、`!*.rs`）和 `a_glob_rg_cannot_take_as_a_type_still_filters`：**在旧实现上同样通过**。前者证明 ccnm 的过滤和 rg 当年的 `--glob` 在这些写法上给出同样的文件；后者覆盖含 `:` 不缩小的分支。
- 门禁数字见 status.json 里 P38 的 evidence。

## 5. 没验的、代价

- **扫描量**：文件名部分是 `**` 的 glob（`src/**`），或调用方同时给了 `type`，rg 会读所有没被忽略的文件（或那一类文件），不匹配的由 ccnm 丢掉。`max_results` 只数留下来的结果，所以提前停止照样有效；最坏情况受 60 秒超时约束。没有在大仓库上量过慢了多少。
- **没对照过的写法**：反斜杠转义、`{}` 里再套 `/`，以及大小写不同的文件系统。
- 其他 rg 版本没测；第 2 节都是 15.2.0 的行为。
