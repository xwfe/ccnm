#!/usr/bin/env python3
"""校验公开协议的 schema 与 fixture 一致；不运行 ccnm rpc，也不证明它的行为与契约一致。

只用标准库。故意不依赖 jsonschema：接续这套计划的前提是"有 Git 和 python3
就能跑"，为一份草案 schema 引入一个第三方包会把这个前提打掉。代价是这里只
实现 JSON Schema 的一个子集，所以 KEYWORDS 之外的关键字一律报错——写了个
本脚本不认识的关键字却被当成通过，比直接报错危险得多。
"""

import json
import re
import sys
from pathlib import Path
from typing import Any

SCHEMA = "docs/protocol/schema/machine-protocol-v1.schema.json"
FIXTURES = "docs/protocol/fixtures"
SPEC = "docs/protocol/machine-protocol-v1.md"

# 本脚本实现的 JSON Schema 关键字。$schema/$id/title/description 只是文档。
KEYWORDS = {
    "$ref", "$defs", "$schema", "$id", "title", "description",
    "type", "const", "enum", "properties", "required", "additionalProperties",
    "items", "minItems", "minLength", "maxLength", "pattern",
    "minimum", "maximum", "oneOf",
}

TYPES = {
    "object": dict, "array": list, "string": str, "boolean": bool,
    "null": type(None), "number": (int, float), "integer": int,
}


def type_ok(value: Any, name: str) -> bool:
    """JSON 类型判断。Python 的 True 是 int 的实例，必须先把 bool 摘出去。"""
    if name not in TYPES:
        return False
    if name == "boolean":
        return isinstance(value, bool)
    if name in ("number", "integer") and isinstance(value, bool):
        return False
    return isinstance(value, TYPES[name])


def resolve(ref: str, root: dict) -> dict:
    if not ref.startswith("#/$defs/") or ref.count("/") != 2:
        raise ValueError("只支持本文件内的 #/$defs/<name> 引用：" + ref)
    name = ref[len("#/$defs/"):]
    if name not in root.get("$defs", {}):
        raise ValueError("引用了不存在的定义：" + ref)
    return root["$defs"][name]


def validate(value: Any, schema: dict, root: dict, where: str) -> list:
    """返回 value 不符合 schema 的原因；空列表表示通过。"""
    errors = []

    def fail(msg: str) -> None:
        errors.append(f"{where}: {msg}")

    unknown = set(schema) - KEYWORDS
    if unknown:
        fail("schema 用了本脚本不认识的关键字：" + ", ".join(sorted(unknown)))
        return errors

    if "$ref" in schema:
        return validate(value, resolve(schema["$ref"], root), root, where)

    if "type" in schema:
        wanted = schema["type"]
        names = [wanted] if isinstance(wanted, str) else list(wanted)
        if not any(type_ok(value, n) for n in names):
            fail(f"类型应为 {'/'.join(names)}，实际是 {type(value).__name__}")
            return errors

    if "const" in schema and value != schema["const"]:
        fail(f"应为常量 {schema['const']!r}，实际是 {value!r}")
    if "enum" in schema and value not in schema["enum"]:
        fail(f"应为 {schema['enum']} 之一，实际是 {value!r}")

    if "oneOf" in schema:
        hits = [s for s in schema["oneOf"] if not validate(value, s, root, where)]
        if len(hits) != 1:
            fail(f"oneOf 命中 {len(hits)} 个分支，应当正好 1 个")

    if isinstance(value, str):
        if "minLength" in schema and len(value) < schema["minLength"]:
            fail(f"字符串短于 {schema['minLength']}")
        if "maxLength" in schema and len(value) > schema["maxLength"]:
            fail(f"字符串长于 {schema['maxLength']}")
        if "pattern" in schema and not re.search(schema["pattern"], value):
            fail(f"不匹配 {schema['pattern']}")

    if isinstance(value, (int, float)) and not isinstance(value, bool):
        if "minimum" in schema and value < schema["minimum"]:
            fail(f"小于最小值 {schema['minimum']}")
        if "maximum" in schema and value > schema["maximum"]:
            fail(f"大于最大值 {schema['maximum']}")

    if isinstance(value, list):
        if "minItems" in schema and len(value) < schema["minItems"]:
            fail(f"数组少于 {schema['minItems']} 项")
        if "items" in schema:
            for i, item in enumerate(value):
                errors += validate(item, schema["items"], root, f"{where}[{i}]")

    if isinstance(value, dict):
        for name in schema.get("required", []):
            if name not in value:
                fail("缺少必填字段 " + name)
        props = schema.get("properties", {})
        for name, item in value.items():
            if name in props:
                errors += validate(item, props[name], root, f"{where}.{name}")
            elif schema.get("additionalProperties") is False:
                fail("多出字段 " + name)

    return errors


def spec_error_codes(text: str) -> dict:
    """从说明文档的错误码表里读出 code -> 名字。JSON-RPC 预定义的那五个没有名字。"""
    codes = {}
    for line in text.splitlines():
        m = re.match(r"^\|\s*`(-?\d+)`\s*\|\s*(?:`([a-z_]+)`\s*\|)?", line)
        if m:
            codes[int(m.group(1))] = m.group(2)
    return codes


def check(root_dir: Path) -> list:
    errors = []
    schema = json.loads((root_dir / SCHEMA).read_text(encoding="utf-8"))
    spec = (root_dir / SPEC).read_text(encoding="utf-8")
    codes = spec_error_codes(spec)
    if not codes:
        return ["说明文档里没找到错误码表"]

    files = sorted((root_dir / FIXTURES).glob("*.json"))
    if not files:
        return ["没有 fixture 可校验"]

    seen_codes = set()
    for path in files:
        name = path.relative_to(root_dir).as_posix()
        try:
            doc = json.loads(path.read_text(encoding="utf-8"))
        except ValueError as exc:
            errors.append(f"{name}: 不是合法 JSON：{exc}")
            continue
        if set(doc) != {"$schema_ref", "$note", "message"}:
            errors.append(name + ": 外层必须正好是 $schema_ref、$note、message")
            continue
        if not doc["$note"].strip():
            errors.append(name + ": $note 不能为空")
        try:
            target = resolve(doc["$schema_ref"], schema)
        except ValueError as exc:
            errors.append(f"{name}: {exc}")
            continue
        errors += [f"{name} {e}" for e in validate(doc["message"], target, schema, "message")]

        error = doc["message"].get("error")
        if isinstance(error, dict) and isinstance(error.get("code"), int):
            code = error["code"]
            seen_codes.add(code)
            if code not in codes:
                errors.append(f"{name}: 错误码 {code} 不在说明文档的表里")

    # 文档里的每个码都要有 fixture：写进表格却没有样例的码，等于没定义。JSON-RPC
    # 预定义的那五个也算——它们的触发条件（id 该填什么、连接断不断）同样要有样例。
    for code, code_name in sorted(codes.items()):
        if code not in seen_codes:
            label = f"{code}（{code_name}）" if code_name else str(code)
            errors.append(f"{SPEC}: 错误码 {label} 没有对应的 fixture")

    return errors


def main() -> int:
    root_dir = Path(__file__).resolve().parents[1]
    try:
        errors = check(root_dir)
    except (OSError, UnicodeError, ValueError) as exc:
        errors = [str(exc)]
    if errors:
        for error in errors:
            print("协议错误：" + error, file=sys.stderr)
        return 1
    total = len(list((root_dir / FIXTURES).glob("*.json")))
    print(f"协议检查通过：{total} 个 fixture 符合 schema，错误码与说明文档一致。")
    print("只是结构检查：证明这几份文件互相自洽，不证明 ccnm rpc 的行为与它们一致。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
