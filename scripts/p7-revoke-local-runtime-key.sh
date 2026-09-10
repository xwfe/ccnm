#!/bin/bash
# p7-authorize-local-runtime.sh 的逆操作：移除本轮追加到既有 ccrun 的公钥行，
# 以及本轮的 root 清单。
#
# 只删清单记录过的那几个字节。别处对 authorized_keys 的并发修改一律保留——不
# 盲目恢复整份备份，因为那会把别人这期间加的行一起抹掉。不动 ccrun 账号本身。
#
# 清理顺序是安装顺序的逆序，所以这个脚本最后跑：先 revert 主组、再 revert 准
# 入，最后才是它。清单目录删掉之后，前两个脚本的前置核对就通不过了。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply) action=$1 ;;
    *) echo 'usage: p7-revoke-local-runtime-key.sh --check|--apply' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || {
    echo 'Run with sudo in the local bing terminal (--check needs it only to read the root record)' >&2
    exit 1; }

home=/Users/ccrun
record=/var/db/ccnm-p7-local-20260910
keys=$home/.ssh/authorized_keys
line_file=$record/appended-line

[[ ! -L $record && -d $record ]] || { echo "Missing this round's record $record" >&2; exit 1; }
[[ $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ -f $line_file && ! -L $line_file ]] || { echo "Missing $line_file" >&2; exit 1; }

# 记的是带前导空行的两行；比对时只认那条非空的 key 行。
appended=$(grep -v '^$' "$line_file")
[[ -n $appended ]]
present=no
[[ -f $keys ]] && ! [[ -L $keys ]] && grep -qxF "$appended" "$keys" && present=yes

# 其他步骤是否还没撤销。它们的清单还在就说明顺序错了。
pending=()
[[ -e $record/local-group-resources.txt ]] && pending+=('primary group (run p7-isolate-local-runtime-group.sh --revert first)')
[[ -e $record/ssh-access-group.txt ]] && pending+=('ssh admission (run p7-grant-local-ssh-access.sh --revert first)')

if [[ $action == --check ]]; then
    printf 'record %s: present\n' "$record"
    printf 'this round key line still in authorized_keys: %s\n' "$present"
    if [[ -f $keys ]]; then
        printf 'authorized_keys: %s non-empty line(s)\n' "$(grep -c . "$keys" || true)"
    else
        echo 'authorized_keys: absent'
    fi
    if ((${#pending[@]})); then
        printf 'BLOCKED, still to undo first:\n'
        printf '  %s\n' "${pending[@]}"
    else
        echo 'apply would remove that one line and delete the record directory.'
    fi
    exit 0
fi

((${#pending[@]} == 0)) || {
    printf 'Undo these first, in this order:\n' >&2
    printf '  %s\n' "${pending[@]}" >&2
    exit 1; }

if [[ $present == yes ]]; then
    # 逐行重写，只丢掉完全相同的那一行；别的行原样保留。写临时文件再原子换过
    # 去，中途失败不会留下半截的 authorized_keys。
    tmp=$(mktemp "$home/.ssh/.authorized_keys.p7.XXXXXX")
    trap 'rm -f "$tmp"' EXIT
    grep -vxF "$appended" "$keys" > "$tmp" || true
    chmod 600 "$tmp"
    chown "504:$(id -g ccrun)" "$tmp"
    mv "$tmp" "$keys"
    trap - EXIT
    ! grep -qxF "$appended" "$keys"
    echo 'This round key line removed; every other line kept.'
else
    echo 'This round key line was not present; nothing removed.'
fi

# 只在本轮确实创建过它们时才回收，不替既有环境做清理。
if [[ -f $record/authorized-keys-created && ! -s $keys ]]; then
    rm "$keys"
    echo 'authorized_keys was created by this round and is now empty; removed.'
fi
if [[ -f $record/ssh-directory ]] && grep -qx created "$record/ssh-directory" \
    && [[ -d $home/.ssh ]] && [[ -z $(ls -A "$home/.ssh") ]]; then
    rmdir "$home/.ssh"
    echo '.ssh was created by this round and is now empty; removed.'
fi

rm -f "$record"/*
rmdir "$record"
echo "Record $record removed. Local resources are back to their pre-round state."
echo 'Verify: id ccrun; dsmemberutil checkmembership -U ccrun -G com.apple.access_ssh'
