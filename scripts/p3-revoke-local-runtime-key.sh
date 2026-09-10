#!/bin/bash
# p3-authorize-local-runtime.sh 的逆操作：移除本轮追加到既有 ccrun 的公钥行与 root 清单。
# 只删清单记录过的那几个字节，别处对 authorized_keys 的并发修改一律保留；
# 不盲目恢复整份备份，不动 ccrun 账号本身。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply) action=$1 ;;
    *) echo 'usage: p3-revoke-local-runtime-key.sh --check|--apply' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || {
    echo 'Run with sudo in the local bing terminal (--check needs it only to read the root record)' >&2
    exit 1; }

home=/Users/ccrun
record=/var/db/ccnm-p3-local-20260908
keys=$home/.ssh/authorized_keys

[[ ! -L $record && -d $record ]] || { echo "Missing this round's record $record" >&2; exit 1; }
[[ $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ -f $record/appended-line ]] || { echo "No appended-line in $record; refusing to guess" >&2; exit 1; }
[[ $(id -u ccrun) == 504 ]]
[[ ! -L $home && ! -L $home/.ssh && ! -L $keys ]]

# 前两个 revert 已各自删掉自己的清单文件；此处只认剩下这几个已知名字。
unexpected=()
while IFS= read -r name; do
    case $name in
        appended-line|ssh-directory|authorized_keys.before|authorized-keys-created) ;;
        *) unexpected+=("$name") ;;
    esac
done < <(ls -A "$record")
if (( ${#unexpected[@]} )); then
    printf 'Unrecognized files in %s; stopping for manual review:\n' "$record" >&2
    printf '  %s\n' "${unexpected[@]}" >&2
    exit 1
fi

line_present=absent
if [[ -f $keys ]] && python3 - "$keys" "$record/appended-line" <<'PY'
import sys
keys=open(sys.argv[1],"rb").read()
line=open(sys.argv[2],"rb").read()
raise SystemExit(0 if line and line in keys else 1)
PY
then line_present=present; fi

if [[ $action == --check ]]; then
    printf 'appended line in authorized_keys: %s\n' "$line_present"
    printf 'authorized_keys created this round: %s\n' \
        "$([[ -f $record/authorized-keys-created ]] && echo yes || echo no)"
    printf 'ssh directory: %s\n' "$(cat "$record/ssh-directory" 2>/dev/null || echo unknown)"
    echo 'Preflight OK; nothing removed.'
    exit 0
fi

if [[ $line_present == present ]]; then
    python3 - "$keys" "$record/appended-line" <<'PY'
import sys
p=sys.argv[1]
keys=open(p,"rb").read()
line=open(sys.argv[2],"rb").read()
# 只切掉记录的那一段，其余字节原样留下。
out=keys.replace(line,b"",1)
open(p,"wb").write(out)
print(f"removed {len(line)} recorded bytes; {len(out)} bytes remain")
PY
else
    echo 'Recorded line not found in authorized_keys; leaving the file untouched'
fi

# 仅当本轮创建且现在为空时才删；原有内容或他人新增一律保留。
if [[ -f $record/authorized-keys-created && -f $keys && ! -s $keys ]]; then
    rm "$keys"
    echo 'removed authorized_keys (created this round, now empty)'
fi
if [[ $(cat "$record/ssh-directory" 2>/dev/null) == created && -d $home/.ssh && -z $(ls -A "$home/.ssh") ]]; then
    rmdir "$home/.ssh"
    echo 'removed .ssh (created this round, now empty)'
fi

rm "$record"/*
rmdir "$record"
echo "Round record removed; ccrun account, its home and any pre-existing authorization are intact."
