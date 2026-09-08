#!/bin/bash
# staff 能穿透 Agent home；仅改变本轮临时账号的主组，不修改个人目录 ACL。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
[[ $# == 1 && $1 == --apply ]] || { echo 'usage: p3-isolate-runtime-group.sh --apply' >&2; exit 2; }
[[ $EUID == 0 && ${SUDO_USER:-} == fodelf ]] || { echo 'Run with sudo in the fodelf terminal' >&2; exit 1; }
home=/Users/ccnmp3test
record=/var/db/ccnm-p3-account-20260908
[[ $(id -u ccnmp3test) == 550 && $(id -g ccnmp3test) == 20 ]]
[[ $(dscl . -read /Users/ccnmp3test NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
[[ ! -L $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ $(cat "$record/state") == created ]]
groups=$(dscl . -list /Groups PrimaryGroupID)
if awk '$1 == "ccnmp3test" || $2 == 550 {found=1} END {exit !found}' <<< "$groups"; then
    echo 'Group name or GID occupied; refusing to modify it' >&2; exit 1
fi
processes=$(ps -axo uid=)
if grep -Eq '^[[:space:]]*550[[:space:]]*$' <<< "$processes"; then
    echo 'Runtime processes still active; close test sessions first' >&2; exit 1
fi
for path in "$home" "$home/.ssh" "$home/.ssh/authorized_keys"; do
    [[ ! -L $path && $(stat -f '%u:%g' "$path") == 550:20 ]]
done
# 在任何变更前记录。失败时保留现场，不盲目重试或删除组。
set -C
printf 'group=ccnmp3test\ngid=550\nprevious_primary_gid=20\n' > "$record/group-resources.txt"
dscl . -create /Groups/ccnmp3test
dscl . -create /Groups/ccnmp3test PrimaryGroupID 550
dscl . -create /Groups/ccnmp3test RealName 'ccnm P3 temporary runtime 20260908'
dscl . -create /Groups/ccnmp3test GeneratedUID "$(uuidgen)"
chgrp 550 "$home" "$home/.ssh" "$home/.ssh/authorized_keys"
dscl . -create /Users/ccnmp3test PrimaryGroupID 550
memberships=$(id -G ccnmp3test)
[[ " $memberships " != *' 20 '* && " $memberships " != *' 80 '* ]]
id ccnmp3test
echo 'Dedicated primary group installed; verify using a fresh SSH connection.'
