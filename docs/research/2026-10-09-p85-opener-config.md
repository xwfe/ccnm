# P85 以你身份生效的项目文件（2026-10-09）

**结论**：专用账号模式下，`ccnm doctor` 多一行 `Files that act as you`（中文 `以你身份生效的文件`）：
- 项目里有这类文件时是"注意"，列出现有的条目；
- 一个都没有时是"正常"；
- Runtime 是旧版本、没报这一项时是"不查"；
- 共用账号不出这一行。

怎么看、怎么硬挡写在[运维手册](../operations.md#专用账号写进项目的配置你打开时按你的身份生效)，本文只记证据。

| 验收 | 做了什么 | 证据 |
| --- | --- | --- |
| P85.1 Runtime 报条目 | 审计回答（`internal runtime-audit`，执行账号自己答）加 `opener_config`。值是固定清单 `runtime::OPENER_CONFIG` 里现在存在的相对名字，符号链接也算，`.git` 不在清单里。字段可缺省，旧 Runtime 不报，读成"不查" | 第 1 节 |
| P85.2 doctor 一行 | `doctor::opener_config_row`，只在专用账号模式下加 | 第 1 节 |
| P85.3 测试 | 两条单元测试，旧代码上红、新代码绿 | 第 1 节 |
| P85.4 文档 | 运维手册写做法（硬挡只写 Linux），生产安全、排错手册指过去，调研第 3 条标已做 | 第 2 节 |

## 1. 测试

两条用例都通过 JSON 构造和读取这个字段，所以在旧代码上能编译，红是真跑出来的：

- `runtime::tests::the_audit_names_what_takes_effect_as_whoever_opens_the_project`：
  - 空项目报 `[]`。旧代码这里报的是 `Null`，测试就红在这一步。
  - 加上 `.vscode`、`.mcp.json`、指向不存在路径的 `.claude` 链接、`.git/hooks`、`README.md` 之后，报 `[".claude", ".mcp.json", ".vscode"]`。
- `doctor::tests::a_dedicated_account_is_told_which_files_act_as_whoever_opens_the_project`：
  - 专用账号、有条目时是 WARN，写明账号名、`present now: …`、`safe.directory`、`chmod`；
  - 没有条目时是 OK；
  - 旧 Runtime（字段删掉）时是 NOTE；
  - 共用账号没有这一行；
  - 中文行名对得上。
  - 旧代码红在 `shown for a dedicated account`。

真实二进制：本机用一份 `runtime_user` 指向自己的临时配置，跑 `ccnm internal runtime-audit`。项目里放了 `.vscode/`、`.mcp.json`、`.git/hooks/`：
- 本次构建报 `opener_config = ['.mcp.json', '.vscode']`；
- 装着的 v0.13.1 不报这个字段。

## 2. 硬挡实测（hpsrv，Debian 13，root 设标志、`ccrun` 试着改）

临时目录 `/home/ccrun/p85/proj` 归 `ccrun`，测完已删。

| root 怎么设 | `ccrun` 做什么 | 结果 |
| --- | --- | --- |
| `chattr +i .vscode`（不带 `-R`） | 改写已有的 `.vscode/tasks.json` | **成功了**：只给目录设挡不住里面已有的文件 |
| 同上 | 在 `.vscode` 里新建 `settings.json`、`mv .vscode`、`rm -rf .vscode`、`chattr -i .vscode`、`chmod 777 .vscode` | 都被拒 |
| `chattr +i .mcp.json` | 改写、`mv`、`rm -f` | 都被拒 |
| `chattr -R +i .vscode .claude` | 改写 `.vscode/tasks.json`、新建 `.claude/settings.json`、`chattr -R -i .vscode` | 都被拒 |
| 不设 | 在项目根新建 `.claude-new` | 成功（还不存在的条目执行账号能建，所以要由 root 先建好再设） |

另外，有不可变条目时 `git add -A` 加 `git commit` 照常成功（只读）。没测切分支或 pull 时 git 改这些文件的情况，运维手册里写的"会失败"是按标志的语义推断的。macOS 的 `chflags` 没测，本机没有免密 sudo，所以手册里不写做法。
