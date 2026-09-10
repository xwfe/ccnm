#!/bin/bash
# 按 root 清单逆序清理本轮 fodelf 临时 Runtime 账号：SSH 授权 → home → 账号 → 独立组 → 清单。
# 只删清单记录过的对象；发现未记录残留或属性不符就停下等人工处理，不递归删除、不猜测。
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

# 清单是唯一授权依据：没有它就无法证明这些对象属于本轮，拒绝动手。
[[ ! -L $record && -d $record ]] || { echo "Missing this round's record $record" >&2; exit 1; }
[[ $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ $(cat "$record/state") == created ]]
grep -qx "account=$account" "$record/resources.txt"
grep -qx "uid=$account_uid" "$record/resources.txt"
grep -qx "home=$home" "$record/resources.txt"

# 目录服务里的实际属性必须与清单一致，否则可能是同名的其他账号。
[[ $(id -u "$account") == "$account_uid" ]]
[[ $(dscl . -read "/Users/$account" NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
[[ $(dscl . -read "/Users/$account" RealName) == *'ccnm P3 temporary runtime 20260908'* ]]
[[ ! -L $home && $(stat -f %u "$home") == "$account_uid" ]]

if ps -axo uid= | grep -Eq "^[[:space:]]*$account_uid[[:space:]]*$"; then
    echo "Runtime processes still active. Close its sessions, then: sudo launchctl bootout user/$account_uid" >&2
    exit 1
fi

# home 下只允许出现清单记录的路径；未记录的文件一律停手，交人工判断。
mapfile -t recorded < <(cat "$record/ssh-resources.txt" 2>/dev/null || true)
declare -A allowed=()
for p in "${recorded[@]}"; do allowed["$p"]=1; done
unexpected=()
while IFS= read -r name; do
    [[ -n $name ]] || continue
    path=$home/$name
    [[ -n ${allowed[$path]:-} ]] || unexpected+=("$path")
done < <(ls -A "$home")
if (( ${#unexpected[@]} )); then
    printf 'Unrecorded leftovers in %s; stopping for manual review:\n' "$home" >&2
    printf '  %s\n' "${unexpected[@]}" >&2
    exit 1
fi

gid_now=$(id -g "$account")
group_recorded=no
if [[ -f $record/group-resources.txt ]]; then
    grep -qx 'group=ccnmp3test' "$record/group-resources.txt"
    grep -qx 'gid=550' "$record/group-resources.txt"
    group_recorded=yes
fi

if [[ $action == --check ]]; then
    echo "Preflight OK; nothing removed."
    printf 'account=%s uid=%s gid=%s home=%s\n' "$account" "$account_uid" "$gid_now" "$home"
    printf 'recorded ssh paths=%s dedicated group recorded=%s\n' "${#recorded[@]}" "$group_recorded"
    exit 0
fi

# 逆序：先撤授权，再删空 home，最后账号与组。
for ((i=${#recorded[@]}-1; i>=0; i--)); do
    p=${recorded[i]}
    [[ $p == "$home"/* ]] || { echo "Recorded path outside home: $p" >&2; exit 1; }
    [[ ! -L $p ]] || { echo "Refusing to follow symlink: $p" >&2; exit 1; }
    if [[ -d $p ]]; then rmdir "$p"; elif [[ -e $p ]]; then rm "$p"; fi
done
[[ -z $(ls -A "$home") ]] || { echo "$home not empty after recorded cleanup" >&2; exit 1; }
rmdir "$home"
dscl . -delete "/Users/$account"

if [[ $group_recorded == yes ]]; then
    # 只有确认没有其他账号还把 550 当主组，才删这个组。
    if dscl . -list /Users PrimaryGroupID | awk '$2 == 550 {found=1} END {exit !found}'; then
        echo 'Another account still uses GID 550; keeping the group' >&2
    else
        dscl . -delete /Groups/ccnmp3test
    fi
fi

rm "$record"/*
rmdir "$record"
echo "Temporary account, home and record removed. fodelf's own account is untouched."
