#!/bin/bash
# sshd 的 PAM account 阶段强制 SACL，标准公钥配置之外还要求 Remote Login 准入。
# 只增删 ccrun 与 com.apple.access_ssh 的直接成员关系；不加 admin、不改主组/UID/密码/防火墙，
# 也不放开其他账号。移除后 ccrun 回到本轮之前的非成员状态。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
[[ $# == 1 && ($1 == --apply || $1 == --revert) ]] || {
    echo 'usage: p3-grant-local-ssh-access.sh --apply|--revert' >&2; exit 2; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || { echo 'Run with sudo in the local bing terminal' >&2; exit 1; }
group=com.apple.access_ssh
record=/var/db/ccnm-p3-local-20260908
manifest=$record/ssh-access-group.txt
[[ $(id -u ccrun) == 504 ]]
[[ ! -L $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ $(dscl . -read "/Groups/$group" PrimaryGroupID) == 'PrimaryGroupID: 399' ]]
nested=$(dscl . -read "/Groups/$group" NestedGroups | tr '\n' ' ')
direct=$(dscl . -read "/Groups/$group" GroupMembership 2>/dev/null | tr '\n' ' ' || true)

# dsmemberutil 有本地缓存，判定前后都刷新，避免拿旧结果当结论。
membership() { dsmemberutil flushcache; dsmemberutil checkmembership -U ccrun -G "$group"; }

if [[ $1 == --apply ]]; then
    [[ $(membership) == 'user is not a member of the group' ]] || {
        echo 'ccrun is already admitted; refusing to record a temporary grant' >&2; exit 1; }
    # 清单先于变更写入；已存在说明上一轮未清理，保留现场不覆盖。
    [[ ! -e $manifest ]] || { echo "Record $manifest already exists; revert or review it first" >&2; exit 1; }
    set -C
    printf 'group=%s\ngid=399\nuser=ccrun\nuid=504\nprevious_direct_member=false\nprevious_nested=%s\nprevious_direct=%s\n' \
        "$group" "$nested" "$direct" > "$manifest"
    set +C
    dseditgroup -o edit -a ccrun -t user "$group"
    [[ $(membership) == 'user is a member of the group' ]]
    echo "ccrun temporarily admitted to $group. Remove with --revert. Record: $manifest"
    exit 0
fi

[[ -f $manifest && ! -L $manifest ]] || { echo "Missing this round's record $manifest" >&2; exit 1; }
grep -qx 'previous_direct_member=false' "$manifest" || {
    echo 'Record does not show ccrun as a non-member before this round; stopping' >&2; exit 1; }
if [[ $(membership) == 'user is a member of the group' ]]; then
    dseditgroup -o edit -d ccrun -t user "$group"
fi
[[ $(membership) == 'user is not a member of the group' ]]
# 基准取本轮清单记录的变更前状态；组的其他成员由别处维护，发现并发修改就保留清单等人工核对。
before_nested=$(sed -n 's/^previous_nested=//p' "$manifest")
before_direct=$(sed -n 's/^previous_direct=//p' "$manifest")
after_nested=$(dscl . -read "/Groups/$group" NestedGroups | tr '\n' ' ')
after_direct=$(dscl . -read "/Groups/$group" GroupMembership 2>/dev/null | tr '\n' ' ' || true)
if [[ $after_nested == "$before_nested" && $after_direct == "$before_direct" ]]; then
    rm "$manifest"
    echo "ccrun admission removed; group restored to its pre-round state."
else
    echo "ccrun admission removed, but the group changed elsewhere. Record kept for review: $manifest" >&2
    exit 1
fi
