# 实施计划与接续

这里记录从 Claude/Codex 内部验证走向 ccnm 独立执行产品的路线，以及交接给独立 Orchestrator 的边界。**计划中的配置、命令和协议名是设计目标，未完成的阶段不代表功能已经存在。**

## 三个入口，只有一份进度

| 文件 | 用途 |
| --- | --- |
| [ROADMAP.md](ROADMAP.md) | 范围、依赖、稳定验收编号和阶段停止点，不记录可变进度 |
| [status.json](status.json) | 当前阶段、完成项、阻塞、证据与下一动作，唯一进度来源 |
| [../research/](../research/) | 脱敏的实测记录；回归 fixture 放在 `tests/fixtures/` |

`AGENTS.md` 是模型入口，`CLAUDE.md` 只指向它。开发命令见 [开发文档](../development.md)。`.gld/planning/state.json` 被忽略，不随 Git 分发；可镜像上述计划，但不能成为第二份事实来源。旧的“在 ccnm 内做多 Agent”计划已被本路线替代，不要恢复执行。

## 每轮执行

1. **读现状。** 核对 Git HEAD、未提交/已暂存修改、`status.json`、当前阶段和证据。HEAD 前进不代表阶段完成；状态与代码冲突时先核对并修正状态，不重做已有实现。没有 `@gld` 的环境也按同一文件接续。
2. **认领一个阶段。** 只在依赖完成后将其置为 `in_progress`，记录 `owner`（模型/任务标识）、`started_at`、`last_updated` 和 `handoff.next_action`。原 owner 已中断时记录交接后再接管，不默认为它已退出。默认不并发执行多个阶段。
3. **按验收编号实施。** 阶段内每个逻辑提交保持可审查。只有对应证据出现后，才把编号加入 `completed_criteria`；把实际命令、环境、结果、commit、限制写入 `evidence`。代码完成但真机验收缺失时不能标记 `completed`。
4. **遇到阻塞就记录。** 写明哪条验收、原因、缺少的环境/授权、解锁动作及本轮已完成内容。能做的离线工作可以继续，但不能用放宽安全检查、复用私人凭据或假数据把门禁变绿。
5. **结束时可接续。** 更新状态、运行计划检查、同步 `@gld` 镜像，提交本轮文件。`current_task` 指向下一项未完成的 ccnm 阶段；`handoff.next_action` 写具体动作，不能只写“继续开发”。阶段结束后默认停止，下一轮才开始后续阶段。

## 状态规则

`pending → in_progress → completed`。`in_progress → blocked → in_progress` 表示确有外部阻塞；普通中断可保持 `in_progress`，但交接时清空 owner 并写明恢复点。已完成项发现证据失效可重新打开，同时撤销依赖它的后续完成状态并记录原因。

- `completed`：所有 `P*.N` 验收编号都有结果，且 evidence 非空、blockers 为空。基线 `P0` 只表示历史内部验证完成，不表示产品发布。
- `blocked`：blockers 非空；写清解除条件，不拿“未轮到”冒充阻塞。
- `pending`：尚未开始；未来步骤统一保持此状态，不能因为前置完成就自动变为进行中。
- 同一时刻最多一个 `in_progress`；`current_task` 指向首个未完成阶段，所有完成/进行中阶段的依赖必须已完成。

`blockers` 每项为 `{"criteria":["P1.3"],"reason":"实际阻塞原因","unblock":"具体解锁动作"}`。暂停交接清空 owner；`pending` 不携带已经实施的验收或证据，已做一部分则保持 `in_progress` 或 `blocked`。

日期使用带时区 ISO 8601；所有引用使用仓库相对路径。证据类型区分 `historical`、`offline`、`real_nodes`、`production_safety`、`review`；其中 `historical` 是引用旧记录，不能写成本轮重跑。

`evidence` 条目示例（示例不是已执行结果）：

```json
{
  "kind": "offline",
  "criteria": ["P1.2"],
  "ref": "docs/research/<实际记录>.md",
  "commit": "<已有实现提交；同提交新增证据可省略>",
  "result": "实际命令、通过/失败/跳过数、平台与未覆盖范围"
}
```

同一提交中的新文件无法自引用最终 hash：使用相对证据路径，由 Git 历史确定提交；不要为了填自身 hash 反复 amend。引用旧提交时填写真实 hash。

## 验证与提交

```bash
python3 scripts/check_plan.py
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests -p 'test_check_plan.py' -v
git diff --check
```

校验器只检查状态结构、依赖、验收编号和文件引用，**不证明实现正确，也不自动更新状态**。产品测试要求见根目录 `AGENTS.md`；文档更新不必为了好看重新消耗订阅跑 Agent。提交前检查暂存 diff，只提交本次明确拥有的文件。

`status.json` 的 `baseline` 是一次带来源的历史基线，不是永远正确的测试数。后续实测写到所属阶段的 evidence，不能把基线数量固定成永不变化的门槛。

## 与 @gld 同步

通过 `planning_manage` 的公开 action 更新 goal/plan，不能手工修改 `.gld` 内部 JSON。阶段用稳定的 `P1…P8` 名称匹配本机 step，不把本机 UUID 当成公共协议。新环境没有记录时，从路线图重建镜像；冲突时先对齐 Git 状态，再同步镜像。

工具写入后必须读回核对；只返回 `ok` 不算同步成功。旧记录字段无法通过公开更新动作对齐时，将旧记录暂停并新建正确镜像，不直接修改工具内部存储。

`task_manage` 只管理当前执行任务的工作区变更。工具返回“写计划成功”或 execution=`completed` 不等于路线图的产品阶段完成。历史计划保留为 paused/cancelled，并注明由本文件替代。
