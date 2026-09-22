# P45：skill 的 frontmatter 照 Claude Code 的读法读（2026-09-22）

零额度，macOS 26.6.2 arm64。共享库那半边和全部差分证据在 toexec 仓库：`evidence/x08-skill-frontmatter/`（做法、语料、每处剩余分歧为什么不跟）。这里只记 ccnm 用上它之后变了什么。

## 行为变更

同一个 SKILL.md，P45 之前的 ccnm 和原生 Claude Code 2.1.278 读出来不一样的地方，按对 ccnm 判定的影响排：

| 写法 | 原生 | P45 之前 | P45 起 |
| --- | --- | --- | --- |
| `disable-model-invocation: yes` / `on` / `1` | 对模型隐藏 | 当没写，模型能调用 | 隐藏 |
| `disable-model-invocation` 写两遍，后一个 `true` | 隐藏 | 先写的赢 | 隐藏，返回开头点名重复的行 |
| ``description: `git` helper`` / `@…` / `*Bold*…` | 正常显示 | skill 被跳过（unsupported YAML construct） | 正常显示 |
| `argument-hint: [issue-number]` | 提示 `issue-number` | 没有提示 | `issue-number` |
| `argument-hint: [filename] [format]` | 原样 | 没有提示（读成一个怪列表） | 原样 |
| `user-invocable:` 空值 / 认不出的字 | 不进 `/` 菜单 | 照样登记成 prompt | 不登记 |
| 严格 YAML 读不了、原生整段丢弃的 frontmatter | 名字、描述、开关全丢 | 宽松读出，不提示 | 仍宽松读出，返回开头写明原生里它不生效 |

最后一行是有意保留的不同：原生丢弃时连 `disable-model-invocation: true` 也跟着丢，宽松读出来对开关是更保守的一侧。

**只改了两条既有测试的输入**，都在造一个"读不了"的 skill：Rust 的 `what_cannot_be_offered_says_why_instead_of_vanishing` 和中立客户端的 `test_without_a_name_the_whole_list_comes_back`。它们用的 `description: &anchor x` 是 YAML 锚点，原生读得了，0.2.0 照原生的第二步也读得了（读成字面文字）——这就是本次的行为变更。换成引号不闭合，两边都仍然读不了。断言本身没动。

## 门禁

依赖当时用一次性环境变量从本地 toexec 解析（那时 toexec 的 tag 还没推；推送之后不需要了）：

```bash
CARGO_NET_GIT_FETCH_WITH_CLI=true GIT_CONFIG_COUNT=1 \
GIT_CONFIG_KEY_0=url.file:///Users/bing/xdw/toexec.insteadOf \
GIT_CONFIG_VALUE_0=https://github.com/xwfe/toexec.git cargo test --workspace
```

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all --check` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | 893 passed / 0 failed（P44 时 890，多的 3 条是本阶段新增） |
| `cargo +1.89 check --workspace --all-targets --locked` | 通过 |
| `python3 -m unittest discover -s tests` | 194 passed |
| `check_plan`、`check_protocol`（38 + 29 个 fixture）、`git diff --check` | 通过 |

**推送**：2026-09-22 按 toexec（连同 tag）→ ccnm → gld 的顺序推送，`Cargo.lock` 里的 `c8cf321` 和远端 tag 对得上，三边 CI 全绿。顺序不能反：先推产品的话，它的 CI 在干净检出上拉不到还不存在的 tag。

## 没做的

- **真实 Host 没跑**：原生的读法是拿 Claude Code 自己的运行时逐字核对的，但"模型会不会因此用上以前被跳过的 skill"没有花额度去验。
- **`name` 的差异**：原生把目录名当 skill 的标识、`name` 只当显示名；ccnm 从 P36 起用 `name`（合法时）当标识。这不是 frontmatter 读法的问题，不在本阶段。
- **gld** 同日也升到了 0.2.0（gld `70cd0f0`）；它自己那半边的 skills 后续（读不了的说原因、用户级附件）记在 gld 的 RFC-0003。
