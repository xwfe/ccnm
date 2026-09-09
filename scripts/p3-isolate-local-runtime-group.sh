#!/bin/bash
# ccrun 的主组是 staff，而 /Users/bing 是 0750 属组 staff，Runtime 因此能穿透 Agent home
# 并读到 0644 的 Codex auth.json。这里只把既有 ccrun 的主组换成专用组，不改个人 home 权限、
# 不加 ACL、不递归修改 ccrun 既有文件，也不动 UID/shell/密码/已有授权。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
[[ $# == 1 && ($1 == --apply || $1 == --revert) ]] || {
    echo 'usage: p3-isolate-local-runtime-group.sh --apply|--revert' >&2; exit 2; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || { echo 'Run with sudo in the local bing terminal' >&2; exit 1; }
home=/Users/ccrun
record=/var/db/ccnm-p3-local-20260908
manifest=$record/local-group-resources.txt
[[ $(id -u ccrun) == 504 ]]
[[ ! -L $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ ! -L $home && $(stat -f '%u:%Lp' "$home") == 504:700 ]]
[[ $(dscl . -read /Users/ccrun NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]

# 改主组不影响已运行进程的既有组身份，留着会让验收结果对不上；拒绝并提示注销用户服务域，
# 不自动 kill、不循环重试。
if ps -axo uid= | grep -Eq '^[[:space:]]*504[[:space:]]*$'; then
    echo 'ccrun processes still active. Close its sessions, then: sudo launchctl bootout user/504' >&2
    exit 1
fi

# 只处理这三条已知路径：mode 已是 0700/0600，改属组是为了避免日后放宽 mode 时缺口重开。
paths=("$home")
[[ -e $home/.ssh ]] && paths+=("$home/.ssh")
[[ -e $home/.ssh/authorized_keys ]] && paths+=("$home/.ssh/authorized_keys")
for p in "${paths[@]}"; do [[ ! -L $p ]]; done

if [[ $1 == --apply ]]; then
    [[ $(id -g ccrun) == 20 ]] || { echo 'ccrun primary group is not staff; refusing to guess' >&2; exit 1; }
    if dscl . -list /Groups PrimaryGroupID | awk '$1 == "ccrun" || $2 == 504 {found=1} END {exit !found}'; then
        echo 'Group name ccrun or GID 504 is occupied; refusing to modify it' >&2; exit 1
    fi
    [[ ! -e $manifest ]] || { echo "Record $manifest already exists; revert or review it first" >&2; exit 1; }
    # 清单先于变更写入；部分失败保留现场供人工核对。
    set -C
    printf 'group=ccrun\ngid=504\nuser=ccrun\nuid=504\nprevious_primary_gid=20\nregrouped_paths=%s\n' \
        "${paths[*]}" > "$manifest"
    set +C
    dscl . -create /Groups/ccrun
    dscl . -create /Groups/ccrun PrimaryGroupID 504
    dscl . -create /Groups/ccrun RealName 'ccnm P3 dedicated runtime group 20260908'
    dscl . -create /Groups/ccrun GeneratedUID "$(uuidgen)"
    chgrp 504 "${paths[@]}"
    dscl . -create /Users/ccrun PrimaryGroupID 504
    dsmemberutil flushcache
    memberships=$(id -G ccrun)
    [[ " $memberships " != *' 20 '* && " $memberships " != *' 80 '* ]]
    id ccrun
    echo "Dedicated primary group installed. Re-run the isolation check over a fresh SSH connection."
    exit 0
fi

[[ -f $manifest && ! -L $manifest ]] || { echo "Missing this round's record $manifest" >&2; exit 1; }
grep -qx 'previous_primary_gid=20' "$manifest" || {
    echo 'Record does not show staff as the previous primary group; stopping' >&2; exit 1; }
[[ $(id -g ccrun) == 504 ]] || { echo 'ccrun primary group is not this round value; stopping' >&2; exit 1; }
dscl . -create /Users/ccrun PrimaryGroupID 20
chgrp 20 "${paths[@]}"
dscl . -delete /Groups/ccrun
dsmemberutil flushcache
[[ $(id -g ccrun) == 20 ]]
rm "$manifest"
id ccrun
echo 'ccrun restored to the staff primary group; dedicated group removed.'
