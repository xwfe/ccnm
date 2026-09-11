#!/bin/bash
# P12 第二步：给 Runtime 执行身份装项目工具链。**不要 root**，就以那个账号跑。
#
#   ssh ccrun@<runtime> 'bash -s -- --check'  < scripts/p12-runtime-toolchain.sh
#   ssh ccrun@<runtime> 'bash -s -- --apply'  < scripts/p12-runtime-toolchain.sh
#
# 装什么：rustup（stable + clippy + rustfmt）和官方 Node 二进制包。全部落在这
# 个账号自己的 home 里（~/.rustup、~/.cargo、~/.local/node-*），不碰 /usr、不
# 碰别的账号。系统级只差一个 C 链接器，那个由 p12-provision-linux-runtime.sh
# 用 apt 装，因为 rustc 链接时要调 `cc`。
#
# ## 最容易踩的一脚：PATH
#
# `exec_command` 的命令跑在一条**非交互** ssh 会话里。Debian 的 ~/.bashrc 第 6
# 行就是
#
#     case $- in *i*) ;; *) return;; esac
#
# 而 rustup 默认把 PATH 追加在文件**末尾**（还有只有 login shell 才读的
# ~/.profile）。两个位置在非交互会话里都不生效，于是 cargo 在 PATH 上找不到，
# ccnm 回的是 "cargo is not installed on the Runtime Node, or is not on its
# PATH"——看着像没装，其实是装了但 PATH 没到。所以这里用 --no-modify-path 自己
# 写，并且把那一块插在那个 return **之前**。
#
# 谁维护：装这一套是 Runtime Node 管理员的事，ccnm 不装、不升级、不代管版本。
# 这个脚本只是把"怎么装得让 exec_command 真能用"写成可重跑的一条命令。
#
# --revert 只删这个脚本自己装的东西：清单记了哪个目录是它建的，别的一律不动。
set -euo pipefail
umask 022

action=
node_version=
while [[ $# -gt 0 ]]; do
    case $1 in
        --check|--apply|--revert) action=$1; shift ;;
        --node-version) node_version=${2:-}; shift 2 ;;
        *) echo "usage: p12-runtime-toolchain.sh --check|--apply|--revert [--node-version vX.Y.Z]" >&2; exit 2 ;;
    esac
done
[[ -n $action ]] || { echo 'usage: p12-runtime-toolchain.sh --check|--apply|--revert [--node-version vX.Y.Z]' >&2; exit 2; }
[[ $(uname -s) == Linux ]] || { echo 'Linux required' >&2; exit 1; }
[[ $EUID != 0 ]] || {
    echo 'Do NOT run this as root: it installs into the Runtime identity own home.' >&2
    echo 'Run it as that account (ssh ccrun@host ...).' >&2
    exit 1; }

record=$HOME/.local/state/ccnm-p12-toolchain
bashrc=$HOME/.bashrc
begin='# >>> ccnm p12 runtime toolchain >>>'
end='# <<< ccnm p12 runtime toolchain <<<'

case $(uname -m) in
    x86_64) rust_arch=x86_64-unknown-linux-gnu; node_arch=linux-x64 ;;
    aarch64) rust_arch=aarch64-unknown-linux-gnu; node_arch=linux-arm64 ;;
    *) echo "unsupported machine $(uname -m)" >&2; exit 1 ;;
esac

have() { command -v "$1" >/dev/null 2>&1; }
present() { [[ -e $1 ]] && echo present || echo absent; }

installed_node_dir() {
    # 清单里记的那个目录名；没有就空。
    [[ -f $record/node-dir ]] && cat "$record/node-dir" || true
}

# ---------------------------------------------------------------- --check

if [[ $action == --check ]]; then
    printf 'account: %s\n' "$(id -un)"
    printf 'home: %s (%s)\n' "$HOME" "$(stat -c %a "$HOME")"
    printf 'record %s: %s\n' "$record" "$(present "$record")"
    printf 'rustup home %s: %s\n' "$HOME/.rustup" "$(present "$HOME/.rustup")"
    printf 'cargo home %s: %s\n' "$HOME/.cargo" "$(present "$HOME/.cargo")"
    recorded_node=$(installed_node_dir)
    printf 'node dir: %s\n' "${recorded_node:-none recorded}"
    printf '.bashrc PATH block: %s\n' \
        "$([[ -f $bashrc ]] && grep -qF "$begin" "$bashrc" && echo present || echo absent)"
    # 这里报的是**本进程**的 PATH，不是 exec_command 会看到的那一条。真正要
    # 问的那句话在最后一行，必须从客户端问。
    for tool in cargo rustc clippy-driver rustfmt node npm git cc make; do
        printf '  %-12s %s\n' "$tool" "$(command -v "$tool" || echo 'not on PATH')"
    done
    echo
    echo 'The only answer that counts (run it on the CLIENT machine):'
    echo "  ssh $(id -un)@<runtime> 'command -v cargo node'"
    exit 0
fi

# ---------------------------------------------------------------- --apply

if [[ $action == --apply ]]; then
    for need in curl tar; do
        have "$need" || { echo "$need is required" >&2; exit 1; }
    done
    have cc || echo 'warning: no cc on PATH; rustc cannot link until the admin installs gcc' >&2
    mkdir -p "$record"

    # -- rustup ---------------------------------------------------------
    if [[ -x $HOME/.cargo/bin/rustup ]]; then
        echo "rustup already installed: $("$HOME/.cargo/bin/rustup" --version 2>/dev/null | head -1)"
    else
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        base=https://static.rust-lang.org/rustup/dist/$rust_arch
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/rustup-init" "$base/rustup-init"
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/rustup-init.sha256" "$base/rustup-init.sha256"
        # 官方 .sha256 的第二列是文件名，而它写的是发布路径，不是本地名字；只
        # 取哈希自己比，别让 sha256sum -c 去对那个名字。
        want=$(awk '{print $1}' "$tmp/rustup-init.sha256")
        got=$(sha256sum "$tmp/rustup-init" | awk '{print $1}')
        [[ $want == "$got" ]] || {
            echo "rustup-init sha256 mismatch: got $got, published $want" >&2; exit 1; }
        chmod +x "$tmp/rustup-init"
        [[ -e $HOME/.rustup ]] || printf 'created\n' > "$record/rustup-home-created"
        [[ -e $HOME/.cargo ]] || printf 'created\n' > "$record/cargo-home-created"
        # minimal + 两个 component：项目的门禁要 fmt 和 clippy，默认 profile 会
        # 多拖一整套文档。
        "$tmp/rustup-init" -y --no-modify-path --profile minimal \
            --default-toolchain stable -c clippy -c rustfmt
        rm -rf "$tmp"
        trap - EXIT
    fi

    # -- node -----------------------------------------------------------
    existing=$(installed_node_dir)
    if [[ -n $existing && -x $HOME/.local/$existing/bin/node ]]; then
        echo "node already installed: $existing"
    else
        if [[ -z $node_version ]]; then
            have python3 || { echo 'need python3 (or --node-version) to resolve the LTS line' >&2; exit 1; }
            node_version=$(curl -fsSL --proto '=https' https://nodejs.org/dist/index.json \
                | python3 -c 'import json,sys
releases = json.load(sys.stdin)
lts = [r for r in releases if r.get("lts")]
if not lts:
    sys.exit("no LTS release in nodejs.org index.json")
print(lts[0]["version"])')
        fi
        [[ $node_version == v*.*.* ]] || { echo "bad --node-version $node_version" >&2; exit 1; }
        dir=node-$node_version-$node_arch
        tarball=$dir.tar.gz
        tmp=$(mktemp -d)
        trap 'rm -rf "$tmp"' EXIT
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/$tarball" \
            "https://nodejs.org/dist/$node_version/$tarball"
        curl -fsSL --proto '=https' --tlsv1.2 -o "$tmp/SHASUMS256.txt" \
            "https://nodejs.org/dist/$node_version/SHASUMS256.txt"
        want=$(awk -v f="$tarball" '$2 == f {print $1}' "$tmp/SHASUMS256.txt")
        [[ -n $want ]] || { echo "$tarball is not in SHASUMS256.txt" >&2; exit 1; }
        got=$(sha256sum "$tmp/$tarball" | awk '{print $1}')
        [[ $want == "$got" ]] || {
            echo "$tarball sha256 mismatch: got $got, published $want" >&2; exit 1; }
        mkdir -p "$HOME/.local"
        tar -xzf "$tmp/$tarball" -C "$HOME/.local"
        printf '%s\n' "$dir" > "$record/node-dir"
        rm -rf "$tmp"
        trap - EXIT
        echo "node installed: ~/.local/$dir"
    fi
    node_dir=$(installed_node_dir)
    [[ -n $node_dir ]] || {
        echo 'no node directory recorded; refusing to write half a PATH' >&2; exit 1; }

    # -- PATH -----------------------------------------------------------
    # 插在 ~/.bashrc 的非交互 return 之前。原文件先备份一份，撤销时按标记块删
    # 除而不是整份恢复：别人（或下一轮）在同一个文件里加的东西要留着。
    if [[ -f $bashrc ]] && grep -qF "$begin" "$bashrc"; then
        echo '.bashrc already has the PATH block; leaving it as it is.'
    else
        [[ -f $bashrc ]] || { : > "$bashrc"; printf 'created\n' > "$record/bashrc-created"; }
        cp "$bashrc" "$record/bashrc.before"
        block=$(mktemp)
        {
            printf '%s\n' "$begin"
            printf '# exec_command 走非交互 ssh；下面几行的 `return` 之后什么都不会执行，\n'
            printf '# 所以项目工具链的 PATH 必须写在这里。删除时整块删掉。\n'
            printf 'PATH="$HOME/.cargo/bin:$HOME/.local/%s/bin:$PATH"\n' "$node_dir"
            printf 'export PATH\n'
            printf '%s\n' "$end"
            printf '\n'
        } > "$block"
        tmp=$(mktemp)
        # 插在第一条 `case $- in` 之前；没有那一行（不是 Debian 的 skel）就插在
        # 文件最前面。两种情况下都是"在任何 return 之前"。
        if grep -qE '^case \$- in' "$bashrc"; then
            awk -v blockfile="$block" '
                !done && /^case \$- in/ {
                    while ((getline line < blockfile) > 0) print line
                    done = 1
                }
                { print }
            ' "$bashrc" > "$tmp"
        else
            cat "$block" "$bashrc" > "$tmp"
        fi
        cat "$tmp" > "$bashrc"
        rm -f "$tmp" "$block"
        echo 'PATH block inserted into ~/.bashrc above the non-interactive return.'
    fi

    echo
    echo 'Installed for this account only:'
    "$HOME/.cargo/bin/cargo" --version || true
    "$HOME/.cargo/bin/rustc" --version || true
    "$HOME/.local/$node_dir/bin/node" --version || true
    "$HOME/.local/$node_dir/bin/npm" --version || true
    echo
    echo 'Now verify from the CLIENT machine that a non-interactive ssh sees them:'
    echo "  ssh $(id -un)@<runtime> 'command -v cargo node npm'"
    exit 0
fi

# ---------------------------------------------------------------- --revert

[[ -d $record ]] || { echo "No record at $record; this script installed nothing here." >&2; exit 1; }

if [[ -f $bashrc ]] && grep -qF "$begin" "$bashrc"; then
    tmp=$(mktemp)
    awk -v b="$begin" -v e="$end" '
        $0 == b { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip { print }
    ' "$bashrc" > "$tmp"
    cat "$tmp" > "$bashrc"
    rm -f "$tmp"
    echo 'PATH block removed from ~/.bashrc; everything else in it kept.'
fi
if [[ -f $record/bashrc-created && -f $bashrc && ! -s $bashrc ]]; then
    rm "$bashrc"
    echo '.bashrc was created by this script and is now empty; removed.'
fi

node_dir=$(installed_node_dir)
if [[ -n $node_dir && -d $HOME/.local/$node_dir ]]; then
    rm -rf "$HOME/.local/$node_dir"
    echo "Removed ~/.local/$node_dir"
fi
if [[ -f $record/rustup-home-created && -d $HOME/.rustup ]]; then
    rm -rf "$HOME/.rustup"
    echo 'Removed ~/.rustup'
fi
if [[ -f $record/cargo-home-created && -d $HOME/.cargo ]]; then
    rm -rf "$HOME/.cargo"
    echo 'Removed ~/.cargo'
fi
rm -rf "$record"
echo "Record $record removed."
echo "Verify: command -v cargo node; ls ~/.rustup ~/.cargo ~/.local 2>&1 | head"
