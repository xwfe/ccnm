#!/bin/bash
# P7.3 第三步：把 ccrun 的主组从 staff 换成专用组。
#
# 为什么必须换：ccrun 的主组是 staff，而 /Users/bing 是 0750、属组 staff。
# Runtime 因此能穿透进 Agent 那侧的 home，P3 实测读到了 0644 的 Codex
# auth.json——68 个叶子文件可读。换成专用组之后同样的复验里可读叶子变成 0。
#
# 只改既有 ccrun 的主组和三条已知路径的属组。不改个人 home 的权限、不加 ACL、
# 不递归改 ccrun 已有文件，也不动 UID/shell/密码/已有授权。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply|--revert) action=$1 ;;
    *) echo 'usage: p7-isolate-local-runtime-group.sh --check|--apply|--revert' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || {
    echo 'Run with sudo in the local bing terminal; do not send the password to the agent' >&2
    exit 1; }

home=/Users/ccrun
record=/var/db/ccnm-p7-local-20260910
manifest=$record/local-group-resources.txt

[[ $(id -u ccrun) == 504 ]] || { echo 'ccrun is not uid 504; refusing to guess' >&2; exit 1; }
[[ ! -L $record && -d $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]] || {
    echo "Missing this round's record $record; run p7-authorize-local-runtime.sh --apply first" >&2
    exit 1; }
[[ ! -L $home && $(stat -f %u "$home") == 504 && $(stat -f %Lp "$home") == 700 ]]
[[ $(dscl . -read /Users/ccrun NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]

# 只处理这三条已知路径：mode 已经是 0700/0600，改属组是为了避免日后放宽 mode
# 时缺口重新打开。
paths=("$home")
[[ -e $home/.ssh ]] && paths+=("$home/.ssh")
[[ -e $home/.ssh/authorized_keys ]] && paths+=("$home/.ssh/authorized_keys")
for p in "${paths[@]}"; do
    [[ ! -L $p ]] || { echo "$p is a symlink; stopping" >&2; exit 1; }
done

current_gid=$(id -g ccrun)
running=$(ps -axo uid= | grep -cE '^[[:space:]]*504[[:space:]]*$' || true)
gid_free=yes
if dscl . -list /Groups PrimaryGroupID | awk '$1 == "ccrun" || $2 == 504 {found=1} END {exit !found}'; then
    gid_free=no
fi

if [[ $action == --check ]]; then
    printf 'ccrun primary gid now: %s (staff is 20)\n' "$current_gid"
    printf 'group name ccrun / GID 504: %s\n' "$([[ $gid_free == yes ]] && echo free || echo OCCUPIED)"
    printf 'ccrun processes running: %s\n' "$running"
    printf 'paths that would be regrouped: %s\n' "${paths[*]}"
    printf 'record %s: %s\n' "$manifest" "$([[ -f $manifest ]] && echo present || echo absent)"
    if [[ $current_gid == 20 && $gid_free == yes && $running == 0 ]]; then
        echo 'apply would create group ccrun (GID 504) and move the account onto it.'
    else
        echo 'apply would refuse: see the lines above for which precondition fails.'
        [[ $running == 0 ]] || echo '  close ccrun sessions first, then: sudo launchctl bootout user/504'
    fi
    exit 0
fi

# 改主组不影响已经在跑的进程的既有组身份，留着会让验收结果对不上。拒绝并提示
# 注销用户服务域，不自动 kill、不循环重试。
if [[ $running != 0 ]]; then
    echo 'ccrun processes still active. Close its sessions, then: sudo launchctl bootout user/504' >&2
    exit 1
fi

if [[ $action == --apply ]]; then
    [[ $current_gid == 20 ]] || { echo 'ccrun primary group is not staff; refusing to guess' >&2; exit 1; }
    [[ $gid_free == yes ]] || { echo 'Group name ccrun or GID 504 is occupied; refusing to modify it' >&2; exit 1; }
    [[ ! -e $manifest ]] || { echo "Record $manifest already exists; revert or review it first" >&2; exit 1; }
    # 清单先于变更写入；部分失败保留现场供人工核对。
    set -C
    printf 'group=ccrun\ngid=504\nuser=ccrun\nuid=504\nprevious_primary_gid=20\nregrouped_paths=%s\n' \
        "${paths[*]}" > "$manifest"
    set +C
    dscl . -create /Groups/ccrun
    dscl . -create /Groups/ccrun PrimaryGroupID 504
    dscl . -create /Groups/ccrun RealName 'ccnm P7.3 dedicated runtime group 20260910'
    dscl . -create /Groups/ccrun GeneratedUID "$(uuidgen)"
    chgrp 504 "${paths[@]}"
    dscl . -create /Users/ccrun PrimaryGroupID 504
    dsmemberutil flushcache
    memberships=$(id -G ccrun)
    # staff(20) 和 admin(80) 都不能再出现，否则隔离没有成立。
    [[ " $memberships " != *' 20 '* && " $memberships " != *' 80 '* ]]
    id ccrun
    echo 'Dedicated primary group installed. Re-run the isolation check over a fresh SSH connection.'
    exit 0
fi

[[ -f $manifest && ! -L $manifest ]] || { echo "Missing this round's record $manifest" >&2; exit 1; }
grep -qx 'previous_primary_gid=20' "$manifest" || {
    echo 'Record does not show staff as the previous primary group; stopping' >&2; exit 1; }
[[ $current_gid == 504 ]] || { echo 'ccrun primary group is not this round value; stopping' >&2; exit 1; }
dscl . -create /Users/ccrun PrimaryGroupID 20
chgrp 20 "${paths[@]}"
dscl . -delete /Groups/ccrun
dsmemberutil flushcache
[[ $(id -g ccrun) == 20 ]]
rm "$manifest"
id ccrun
echo 'ccrun restored to the staff primary group; dedicated group removed.'
