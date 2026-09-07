#!/usr/bin/env python3
"""只读检查计划状态；不运行 Agent、不改文件，也不判断证据内容是否充分。"""

import json
import re
import sys
from datetime import datetime
from pathlib import Path
from typing import Any


STATUSES = {"pending", "in_progress", "blocked", "completed"}
KINDS = {"historical", "offline", "real_nodes", "production_safety", "review"}
ENTRY_DOCS = (
    "AGENTS.md", "CLAUDE.md", "docs/plan/README.md",
    "docs/plan/ROADMAP.md", "docs/development.md",
)


def nonempty(value: Any) -> bool:
    return isinstance(value, str) and bool(value.strip())


def timestamp(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).utcoffset() is not None
    except ValueError:
        return False


def local_file(root: Path, value: Any) -> bool:
    """只检查仓库内相对文件引用，拒绝越界及通过 symlink 逃逸。"""
    if not nonempty(value) or "\\" in value:
        return False
    path = Path(value)
    if path.is_absolute() or ".." in path.parts:
        return False
    try:
        target = (root / path).resolve()
        target.relative_to(root.resolve())
        return target.is_file()
    except (OSError, RuntimeError, ValueError):
        return False


def unique_object(pairs: Any) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("JSON 重复字段：" + key)
        result[key] = value
    return result


def validate(state: Any, roadmap: str, root: Path) -> list:
    errors = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            errors.append(message)

    def names(value: Any, label: str) -> list:
        if not isinstance(value, list) or not all(nonempty(v) for v in value):
            errors.append(label + " 必须是字符串列表")
            return []
        require(len(value) == len(set(value)), label + " 有重复项")
        return value

    if not isinstance(state, dict):
        return ["status.json 顶层必须是对象"]
    require(type(state.get("schema_version")) is int and state["schema_version"] == 1,
            "schema_version 必须为 1")
    require(nonempty(state.get("plan_id")), "缺少 plan_id")
    require(timestamp(state.get("last_updated")), "last_updated 必须有时区")
    baseline = state.get("baseline")
    if not isinstance(baseline, dict):
        errors.append("缺少 baseline")
    else:
        require(local_file(root, baseline.get("source")), "baseline.source 文件不存在或越界")
        require(baseline.get("verification_kind") == "historical", "baseline 必须标明 historical")
        require(bool(re.fullmatch(r"[0-9a-f]{7,40}", str(baseline.get("implementation_commit", "")))),
                "baseline 缺少实现 commit")

    phase_ids = re.findall(r"^### (P\d+) —", roadmap, re.M)
    criterion_ids = re.findall(r"^- \*\*(P\d+\.\d+)\*\*", roadmap, re.M)
    require(bool(phase_ids) and len(phase_ids) == len(set(phase_ids)), "ROADMAP 阶段缺失或重复")
    require(len(criterion_ids) == len(set(criterion_ids)), "ROADMAP 验收编号重复")
    require(all(c.split(".")[0] in phase_ids for c in criterion_ids), "ROADMAP 验收编号没有所属阶段")
    tasks = state.get("tasks")
    if not isinstance(tasks, list) or not all(isinstance(t, dict) for t in tasks):
        return errors + ["tasks 必须是对象列表"]
    require([t.get("id") for t in tasks] == phase_ids, "tasks 必须与 ROADMAP 阶段同序且不缺失/重复")
    if [t.get("id") for t in tasks] != phase_ids:
        return errors
    by_id = {t["id"]: t for t in tasks}
    active = []

    for index, task in enumerate(tasks):
        tid = task["id"]
        status = task.get("status")
        require(isinstance(status, str) and status in STATUSES, tid + " 状态无效")
        require(nonempty(task.get("title")), tid + " 缺少 title")
        require(timestamp(task.get("last_updated")), tid + " last_updated 必须有时区")
        require("owner" in task and (task["owner"] is None or nonempty(task["owner"])), tid + " owner 无效")
        require("started_at" in task and (task["started_at"] is None or timestamp(task["started_at"])),
                tid + " started_at 无效")
        deps = names(task.get("depends_on"), tid + ".depends_on")
        expected_deps = [] if index == 0 else [phase_ids[index - 1]]
        require(deps == expected_deps, tid + " 不符合路线图的顺序依赖")
        if status in ("in_progress", "blocked", "completed"):
            require(all(by_id.get(d, {}).get("status") == "completed" for d in deps), tid + " 前置阶段未完成")
        if status in ("in_progress", "blocked"):
            require(timestamp(task.get("started_at")), tid + " 开始后必须记录 started_at")
        if status == "in_progress":
            active.append(tid)
        else:
            require(task.get("owner") is None, tid + " 非进行中阶段不能保留 owner")

        expected = {c for c in criterion_ids if c.startswith(tid + ".")}
        require(bool(expected), tid + " 没有验收编号")
        completed = set(names(task.get("completed_criteria"), tid + ".completed_criteria"))
        require(completed <= expected, tid + " 包含未知验收编号")
        evidence = task.get("evidence")
        covered = set()
        if not isinstance(evidence, list):
            errors.append(tid + " evidence 必须是列表")
            evidence = []
        for item in evidence:
            if not isinstance(item, dict):
                errors.append(tid + " evidence 条目必须是对象")
                continue
            ids = names(item.get("criteria"), tid + ".evidence.criteria")
            require(bool(ids) and set(ids) <= expected, tid + " evidence 的验收编号无效")
            require(isinstance(item.get("kind"), str) and item["kind"] in KINDS, tid + " evidence.kind 无效")
            require(local_file(root, item.get("ref")), tid + " evidence.ref 文件不存在或越界")
            require(nonempty(item.get("result")), tid + " evidence 缺少实际结果/限制")
            if "commit" in item:
                require(bool(re.fullmatch(r"[0-9a-f]{7,40}", str(item["commit"]))), tid + " evidence.commit 无效")
            covered.update(ids)
        require(completed <= covered, tid + " 已勾选验收缺少 evidence")

        blockers = task.get("blockers")
        if not isinstance(blockers, list):
            errors.append(tid + " blockers 必须是列表")
            blockers = []
        for item in blockers:
            if not isinstance(item, dict):
                errors.append(tid + " blocker 必须写 criteria/reason/unblock")
                continue
            ids = names(item.get("criteria"), tid + ".blocker.criteria")
            require(bool(ids) and set(ids) <= expected, tid + " blocker 验收编号无效")
            require(nonempty(item.get("reason")) and nonempty(item.get("unblock")), tid + " blocker 缺少原因/解锁动作")
        if status == "completed":
            require(completed == expected and bool(evidence), tid + " 完成必须覆盖全部验收并有证据")
            require(not blockers, tid + " 完成后不能仍有 blocker")
        if status == "blocked":
            require(bool(blockers), tid + " blocked 必须记录 blocker")
        if status == "pending":
            require(not completed and not evidence and not blockers and task.get("started_at") is None,
                    tid + " pending 不应有已执行内容")

    incomplete = [t["id"] for t in tasks if t.get("status") != "completed"]
    expected_current = incomplete[0] if incomplete else None
    require(state.get("current_task") == expected_current, "current_task 必须指向首个未完成阶段，全部完成时为 null")
    require(len(active) <= 1, "同时存在多个 in_progress")
    require(not active or active == [expected_current], "in_progress 必须与 current_task 一致")
    handoff = state.get("handoff")
    require(isinstance(handoff, dict) and nonempty(handoff.get("next_action")), "缺少具体 handoff.next_action")
    return errors


def check_links(root: Path) -> list:
    errors = []
    for name in ENTRY_DOCS:
        path = root / name
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            errors.append(str(exc))
            continue
        for link in re.findall(r"\[[^\]]+\]\(([^\s)]+)\)", text):
            if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", link) or link.startswith("#"):
                continue
            target = (path.parent / link.split("#", 1)[0]).resolve()
            try:
                target.relative_to(root.resolve())
                if not target.exists():
                    errors.append(name + " 链接不存在：" + link)
            except ValueError:
                errors.append(name + " 链接越界：" + link)
    return errors


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    try:
        state = json.loads((root / "docs/plan/status.json").read_text(encoding="utf-8"),
                           object_pairs_hook=unique_object)
        roadmap = (root / "docs/plan/ROADMAP.md").read_text(encoding="utf-8")
        errors = validate(state, roadmap, root) + check_links(root)
    except (OSError, UnicodeError, ValueError) as exc:
        errors = [str(exc)]
    if errors:
        for error in errors:
            print("计划错误：" + error, file=sys.stderr)
        return 1
    done = sum(t["status"] == "completed" for t in state["tasks"])
    print(f"计划检查通过：{done}/{len(state['tasks'])} 阶段完成（含历史基线）；下一阶段 {state['current_task']}")
    print("仅结构与引用检查，不代表产品验收通过。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
