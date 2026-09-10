#!/bin/bash
# 按 root 清单逆序清理本轮 fodelf 临时 Runtime 账号：SSH 授权 → home → 账号 → 独立组 → 清单。
# 只删清单记录过的对象；发现未记录残留或属性不符就停下等人工处理，不递归删除、不猜测。
# 中途失败后可以直接重跑：账号还在就从头做，账号已经删掉就只补做剩下的组和清单；
# 已删的记录路径会被跳过，前置核对每次重新做。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply) action=$1 ;;
    *) echo 'usage: p3-cleanup-runtime-user.sh --check|--apply' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
operator=${SUDO_USER:-$(id -un)}
[[ $operator == fodelf ]] || { echo 'Run only in the fodelf login terminal' >&2; exit 1; }
# 两个动作都要 root：清单是 root 所有的 0700，连核对都读不到。--check 仍然只读，不改任何东西。
[[ $EUID == 0 ]] || {
    echo 'Use sudo in your login terminal (--check needs it only to read the root record); do not send the password to the agent' >&2
    exit 1; }

account=ccnmp3test
account_uid=550
home=/Users/ccnmp3test
record=/var/db/ccnm-p3-account-20260908

# macOS 给 /Users 和 /var/db 都设了 sunlnk（system no-unlink）：连 root 都不能删除或改名
# 其中的条目，rmdir 直接报 "Operation not permitted"——本轮实际踩到两次。/var/db 可以临时
# 摘掉这个 flag（下面这个函数），/Users 不行，它列在 SIP 的 rootless.conf 里，chflags 本身
# 就被拒绝，那条路径改用 sysadminctl，见下文。
# 摘 flag 后立刻装回并复核装回成功；trap 保证脚本异常退出也不会把系统目录留在无保护状态。
# 只动这一个 flag，不碰权限、属主，不递归。
sunlnk_set() {
    stat -f %Sf "$1" | grep -qw sunlnk
}

rmdir_in_protected_parent() {
    local target=$1
    local parent=${target%/*}
    # 父目录带 sunlnk 不等于一定删不掉，先直接试，别为了猜测去动系统目录的 flag。
    if rmdir "$target" 2>/dev/null; then
        return
    fi
    if ! sunlnk_set "$parent"; then
        rmdir "$target"     # 不是 sunlnk 的问题，让真实错误打出来
        return
    fi
    # trap 在设置时就把路径展开进字符串：函数的 local 变量到脚本退出时已经不在了。
    trap "chflags sunlnk '$parent'" EXIT
    if ! chflags nosunlnk "$parent" 2>/dev/null; then
        trap - EXIT
        echo "Cannot remove $target: $parent has sunlnk and SIP refuses to clear it" >&2
        exit 1
    fi
    rmdir "$target"
    chflags sunlnk "$parent"
    trap - EXIT
    sunlnk_set "$parent" || { echo "Failed to restore sunlnk on $parent" >&2; exit 1; }
}

# 在删掉目录内容之前，先确认这个目录最后删得掉。非空目录 rmdir 报 "Directory not
# empty"，父目录不让删则报 "Operation not permitted"，两者可以区分，所以这是一次
# 无损探测：本轮就吃过亏——清单内容删干净了，却留下一个删不掉的空目录。
probe_removable() {
    local err
    if err=$(rmdir "$1" 2>&1); then
        return 0            # 本来就是空的，顺手删掉了
    fi
    case $err in
        *'not empty'*) return 0 ;;
        *) echo "Cannot remove $1: $err" >&2; return 1 ;;
    esac
}

# 清单是唯一授权依据：没有它就无法证明这些对象属于本轮，拒绝动手。
[[ ! -L $record && -d $record ]] || { echo "Missing this round's record $record" >&2; exit 1; }
[[ $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ $(cat "$record/state") == created ]]
grep -qx "account=$account" "$record/resources.txt"
grep -qx "uid=$account_uid" "$record/resources.txt"
grep -qx "home=$home" "$record/resources.txt"

# 账号可能在上一次执行里已经删掉了，只剩后面几步没做完。两种情况都要能接着跑，
# 所以先判断它还在不在，再决定核对哪些东西。
account_present=no
if dscl . -read "/Users/$account" RecordName >/dev/null 2>&1; then
    account_present=yes
fi

recorded_file=$record/ssh-resources.txt
[[ -f $recorded_file ]] || { echo "Missing $recorded_file; refusing to guess what this round installed" >&2; exit 1; }
recorded_count=$(grep -c . "$recorded_file")
gid_now=gone

if [[ $account_present == yes ]]; then
    # 目录服务里的实际属性必须与清单一致，否则可能是同名的其他账号。
    [[ $(id -u "$account") == "$account_uid" ]]
    [[ $(dscl . -read "/Users/$account" NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
    [[ $(dscl . -read "/Users/$account" RealName) == *'ccnm P3 temporary runtime 20260908'* ]]
    [[ ! -L $home && $(stat -f %u "$home") == "$account_uid" ]]
    gid_now=$(id -g "$account")

    if ps -axo uid= | grep -Eq "^[[:space:]]*$account_uid[[:space:]]*$"; then
        echo "Runtime processes still active. Close its sessions, then: sudo launchctl bootout user/$account_uid" >&2
        exit 1
    fi

    # home 下只允许出现清单记录的路径；未记录的文件一律停手，交人工判断。
    # 用户在 macOS 终端执行，那里的 /bin/bash 是 3.2，只用它支持的语法。
    unexpected=""
    while IFS= read -r name; do
        [[ -n $name ]] || continue
        if ! grep -qxF "$home/$name" "$recorded_file"; then
            unexpected="$unexpected$home/$name
"
        fi
    done < <(ls -A "$home")
    if [[ -n $unexpected ]]; then
        printf 'Unrecorded leftovers in %s; stopping for manual review:\n' "$home" >&2
        printf '%s' "$unexpected" | sed 's/^/  /' >&2
        exit 1
    fi
else
    # 账号没了，home 就不该还在：那说明删除只做了一半，交人工看，不自己猜。
    [[ ! -e $home ]] || { echo "$account is gone but $home still exists" >&2; exit 1; }
fi

group_recorded=no
if [[ -f $record/group-resources.txt ]]; then
    grep -qx 'group=ccnmp3test' "$record/group-resources.txt"
    grep -qx 'gid=550' "$record/group-resources.txt"
    group_recorded=yes
fi

if [[ $action == --check ]]; then
    echo "Preflight OK; nothing removed."
    printf 'account=%s present=%s uid=%s gid=%s home=%s\n' \
        "$account" "$account_present" "$account_uid" "$gid_now" "$home"
    printf 'recorded ssh paths=%s dedicated group recorded=%s\n' "$recorded_count" "$group_recorded"
    if [[ $account_present == yes ]]; then
        printf 'apply will delete the account and home with: sysadminctl -deleteUser %s\n' "$account"
    else
        printf 'account and home are already gone; apply will only finish the group and the record\n'
    fi
    if sunlnk_set "${record%/*}"; then
        printf 'if a plain rmdir is refused, apply will briefly clear sunlnk on %s and restore it\n' \
            "${record%/*}"
    fi
    exit 0
fi

# 逆序：先撤授权（文件在目录之前），再删账号与 home，最后组和清单。
if [[ $account_present == yes ]]; then
    # 从文件重定向而非管道，循环体才留在当前 shell，里面的 exit 才有效。
    reversed=$(sed -n '1!G;h;$p' "$recorded_file")
    while IFS= read -r p; do
        [[ -n $p ]] || continue
        [[ $p == "$home"/* ]] || { echo "Recorded path outside home: $p" >&2; exit 1; }
        [[ ! -L $p ]] || { echo "Refusing to follow symlink: $p" >&2; exit 1; }
        if [[ -d $p ]]; then rmdir "$p"; elif [[ -e $p ]]; then rm "$p"; fi
    done <<< "$reversed"
    [[ -z $(ls -A "$home") ]] || { echo "$home not empty after recorded cleanup" >&2; exit 1; }

    # /Users 列在 SIP 的 rootless.conf 里，连 root 都改不了它的 flag（实测
    # chflags: /Users: Operation not permitted），所以摘 sunlnk 这条路对 home 不成立。
    # 删账号和 home 改用 Apple 自己的 sysadminctl：它带 SIP entitlement，是系统设置里
    # 删用户走的同一条路，一步同时删目录记录和 home。上面已经核对过 home 为空、属性与
    # 清单一致，这里不会连带删到别的东西。不加 -secure（安全擦除对空目录没有意义）。
    sysadminctl -deleteUser "$account" || true
    dscacheutil -flushcache
    # sysadminctl 失败时也可能返回 0，只认实际结果：账号记录和 home 都必须消失。
    if dscl . -read "/Users/$account" RecordName >/dev/null 2>&1; then
        echo "sysadminctl did not remove $account. If it asked for administrator authentication," >&2
        echo "run this in the fodelf login terminal: sudo sysadminctl -deleteUser $account interactive" >&2
        exit 1
    fi
    [[ ! -e $home ]] || { echo "$home still exists after -deleteUser" >&2; exit 1; }
    # -deleteUser 有时会把 home 挪进 Deleted Users（或打包成 dmg）而不是删掉，那样并没有
    # 归零。发现就停手并保留清单，交人工处理，不自己去动那个目录。
    if ls -d "/Users/Deleted Users/$account"* >/dev/null 2>&1; then
        echo "-deleteUser left this round's home under /Users/Deleted Users; not zeroed" >&2
        exit 1
    fi
fi

if [[ $group_recorded == yes ]]; then
    if ! dscl . -read /Groups/ccnmp3test PrimaryGroupID >/dev/null 2>&1; then
        # sysadminctl -deleteUser 把用户的主组一并删掉了，这里没事可做。
        echo 'Dedicated group already removed together with the account'
    # 只有确认没有其他账号还把 550 当主组，才删这个组。
    elif dscl . -list /Users PrimaryGroupID | awk '$2 == 550 {found=1} END {exit !found}'; then
        echo 'Another account still uses GID 550; keeping the group' >&2
    else
        dscl . -delete /Groups/ccnmp3test
    fi
fi

probe_removable "$record" || exit 1
if [[ -d $record ]]; then
    rm "$record"/*
    rmdir_in_protected_parent "$record"
fi
echo "Temporary account, home and record removed. fodelf's own account is untouched."
