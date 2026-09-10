#!/bin/bash
# P7.3 第二步：让 ccrun 能通过 SSH 登录进来。
#
# 标准的公钥配置在这台机器上不够用：sshd 的 PAM account 阶段强制 SACL
# (service access control list)，说白了就是"系统设置里勾了远程登录的那份名
# 单"。不在名单里的账号，密钥再对也会被拒——现象是连接建立后立刻断开，看着
# 像密钥不对，实际是准入没给。
#
# 只增删 ccrun 与 com.apple.access_ssh 的**直接**成员关系；不加 admin、不改
# 主组/UID/密码/防火墙，也不放开其他账号。--revert 之后 ccrun 回到本轮之前的
# 非成员状态。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply|--revert) action=$1 ;;
    *) echo 'usage: p7-grant-local-ssh-access.sh --check|--apply|--revert' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || {
    echo 'Run with sudo in the local bing terminal; do not send the password to the agent' >&2
    exit 1; }

group=com.apple.access_ssh
record=/var/db/ccnm-p7-local-20260910
manifest=$record/ssh-access-group.txt

[[ $(id -u ccrun) == 504 ]] || { echo 'ccrun is not uid 504; refusing to guess' >&2; exit 1; }
[[ ! -L $record && -d $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]] || {
    echo "Missing this round's record $record; run p7-authorize-local-runtime.sh --apply first" >&2
    exit 1; }
[[ $(dscl . -read "/Groups/$group" PrimaryGroupID) == 'PrimaryGroupID: 399' ]]
nested=$(dscl . -read "/Groups/$group" NestedGroups | tr '\n' ' ')
direct=$(dscl . -read "/Groups/$group" GroupMembership 2>/dev/null | tr '\n' ' ' || true)

# dsmemberutil 有本地缓存，判定前后都刷新，避免拿旧结果当结论。
membership() { dsmemberutil flushcache; dsmemberutil checkmembership -U ccrun -G "$group"; }
state=$(membership)

if [[ $action == --check ]]; then
    printf 'group=%s gid=399\n' "$group"
    printf 'ccrun: %s\n' "$state"
    printf 'direct members now: %s\n' "${direct:-<none>}"
    printf 'record %s: %s\n' "$manifest" "$([[ -f $manifest ]] && echo present || echo absent)"
    if [[ $state == 'user is not a member of the group' ]]; then
        echo 'apply would add ccrun as a single direct member; nothing else changes.'
    else
        echo 'ccrun is already a member; apply would refuse. --revert removes only this round grant.'
    fi
    exit 0
fi

if [[ $action == --apply ]]; then
    [[ $state == 'user is not a member of the group' ]] || {
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
    echo 'Next: p7-isolate-local-runtime-group.sh --check'
    exit 0
fi

[[ -f $manifest && ! -L $manifest ]] || { echo "Missing this round's record $manifest" >&2; exit 1; }
grep -qx 'previous_direct_member=false' "$manifest" || {
    echo 'Record does not show ccrun as a non-member before this round; stopping' >&2; exit 1; }
if [[ $state == 'user is a member of the group' ]]; then
    dseditgroup -o edit -d ccrun -t user "$group"
fi
[[ $(membership) == 'user is not a member of the group' ]]
# 基准取本轮清单记录的变更前状态；组的其他成员由别处维护，发现并发修改就保留
# 清单等人工核对，不替别人把他们的改动抹掉。
before_nested=$(sed -n 's/^previous_nested=//p' "$manifest")
before_direct=$(sed -n 's/^previous_direct=//p' "$manifest")
after_nested=$(dscl . -read "/Groups/$group" NestedGroups | tr '\n' ' ')
after_direct=$(dscl . -read "/Groups/$group" GroupMembership 2>/dev/null | tr '\n' ' ' || true)
if [[ $after_nested == "$before_nested" && $after_direct == "$before_direct" ]]; then
    rm "$manifest"
    echo 'ccrun admission removed; group restored to its pre-round state.'
else
    echo "ccrun admission removed, but the group changed elsewhere. Record kept for review: $manifest" >&2
    exit 1
fi
