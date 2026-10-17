#!/usr/bin/env python3
"""P104：rg 对"点名的文件"和 search_text 用的那几条排除规则是什么关系（零额度，只跑 rg）。

要量的：search_text 照搬到 rg 的约束——`.gitignore`、`--no-hidden`、`--glob '!.*'`、
`--glob '!.git'`、`--type`（含给 glob 缩范围的 `--type-add`）、排除用的 glob、`--no-follow`——
在 `path` 是一个文件时还管不管用；点名的二进制文件怎么搜；点名的目录是指向 `.git` 的符号
链接时会怎样。每一行的参数就是 `search.rs` 的 `Plan::command` 拼出来的那几个。

用法：
  python3 docs/research/probes/p104-rg-explicit-file.py [rg 路径，默认 PATH 里的 rg]

在一个临时目录里造文件、跑完删掉，什么都不写到别处。
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

BASE = ["--json", "--no-config", "--no-follow", "--no-hidden", "--case-sensitive", "--fixed-strings"]
EXCLUDE = ["--glob", "!.*", "--glob", "!.git"]

CASES = [
    # (说明, 额外参数, 点名的路径)
    ("普通文件", [], "src/main.rs"),
    (".gitignore 排除的文件", [], "src/gen.rs"),
    (".gitignore 排除的目录里的文件", [], "ignored/x.rs"),
    ("dotfile", [], ".hidden"),
    ("点开头目录里的文件", [], ".github/ci.yml"),
    (".git 里的文件", [], ".git/config"),
    ("指向文件的符号链接", [], "link.rs"),
    ("指向 .git/config 的符号链接", [], "gitlink"),
    ("--type=py 点名 .rs", ["--type=py"], "src/main.rs"),
    ("glob 缩范围的 --type-add *.py 点名 .rs", ["--type-add=ccnmglob:*.py", "--type=ccnmglob"], "src/main.rs"),
    ("排除 glob !*.rs 点名 .rs", ["--glob", "!*.rs"], "src/main.rs"),
    ("二进制文件（NUL 在第 6 字节）", [], "blob.bin"),
    ("目录：.gitignore 排除的", [], "ignored"),
    ("目录：.git", [], ".git"),
    ("目录：指向 .git 的符号链接", [], "gitdir"),
    ("目录：指向 src 的符号链接", [], "srclink"),
    ("对照：从根目录遍历", [], "./"),
]


def build(root: str) -> None:
    def write(rel: str, data: bytes) -> None:
        path = os.path.join(root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "wb") as f:
            f.write(data)

    write(".gitignore", b"ignored/\ngen.rs\n")
    write("src/main.rs", b"needle main\n")
    write("src/gen.rs", b"needle gen\n")
    write("ignored/x.rs", b"needle ignored\n")
    write(".hidden", b"needle dot\n")
    write(".github/ci.yml", b"needle ci\n")
    write(".git/config", b"needle git\n")
    write("blob.bin", b"needle" + b"\0" * 64 + b"needle secret")
    os.symlink("src/main.rs", os.path.join(root, "link.rs"))
    os.symlink(".git/config", os.path.join(root, "gitlink"))
    os.symlink(".git", os.path.join(root, "gitdir"))
    os.symlink("src", os.path.join(root, "srclink"))


def run(rg: str, root: str, extra: list, path: str) -> str:
    argv = [rg, *BASE, *extra, *EXCLUDE, "--", "needle", path]
    out = subprocess.run(argv, cwd=root, capture_output=True, text=True)
    hits = []
    binary = None
    for line in out.stdout.splitlines():
        event = json.loads(line)
        data = event.get("data", {})
        if event["type"] == "match":
            hits.append(f'{data["path"].get("text")}: {data["lines"].get("text", "").rstrip()!r}')
        if event["type"] == "end" and data.get("binary_offset") is not None:
            binary = data["binary_offset"]
    note = f"（end 里 binary_offset={binary}）" if binary is not None else ""
    return ("; ".join(hits) or f"没搜（退出 {out.returncode}）") + note


def main() -> int:
    rg = sys.argv[1] if len(sys.argv) > 1 else shutil.which("rg")
    if not rg:
        print("找不到 rg", file=sys.stderr)
        return 2
    version = subprocess.run([rg, "--version"], capture_output=True, text=True).stdout.splitlines()[0]
    print(f"{version}，固定参数：{' '.join(BASE + EXCLUDE)}\n")
    print("| 点名的 | 额外参数 | 结果 |")
    print("| --- | --- | --- |")
    root = tempfile.mkdtemp(prefix="p104-rg-")
    try:
        build(root)
        for label, extra, path in CASES:
            print(f"| {label}（`{path}`） | `{' '.join(extra)}` | {run(rg, root, extra, path)} |")
    finally:
        shutil.rmtree(root)
    return 0


if __name__ == "__main__":
    sys.exit(main())
