#!/bin/bash
# 仅用于已授权的 fodelf P3 实测；不安装服务、不写 SSH 授权、不修改既有账号。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077

case "${1:-}" in
    --check|--create) action=$1 ;;
    *) echo 'usage: p3-create-runtime-user.sh --check|--create' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
operator=${SUDO_USER:-$(id -un)}
[[ $operator == fodelf ]] || { echo 'Run only in the fodelf login terminal' >&2; exit 1; }
if [[ $action == --create && $EUID != 0 ]]; then
    echo 'Use sudo in your login terminal; do not send the password to the agent' >&2
    exit 1
fi

account=ccnmp3test
account_uid=550
home=/Users/ccnmp3test
record=/var/db/ccnm-p3-account-20260908
users=$(dscl . -list /Users UniqueID)
if awk -v name="$account" -v uid="$account_uid" '$1 == name || $2 == uid {found=1} END {exit !found}' <<< "$users"; then
    echo 'Account name or UID already exists; refusing to modify it' >&2
    exit 1
fi
for path in "$home" "$record"; do
    [[ ! -e $path && ! -L $path ]] || { echo "Already exists: $path" >&2; exit 1; }
done
[[ $action == --create ]] || { echo 'Preflight OK; no changes made'; exit 0; }

# 先留 root 所有的清单；部分失败保留现场，不盲目删除可能已被使用的账号。
mkdir -m 700 "$record"
printf 'account=%s\nuid=%s\nhome=%s\n' "$account" "$account_uid" "$home" > "$record/resources.txt"
trap 'echo "Preparation failed; retain and inspect $record before recovery" >&2' ERR
dscl . -create "/Users/$account"
dscl . -create "/Users/$account" RealName 'ccnm P3 temporary runtime 20260908'
dscl . -create "/Users/$account" UniqueID "$account_uid"
dscl . -create "/Users/$account" PrimaryGroupID 20
dscl . -create "/Users/$account" UserShell /bin/zsh
dscl . -create "/Users/$account" NFSHomeDirectory "$home"
dscl . -create "/Users/$account" Password '*'
mkdir -m 700 "$home"
chown "$account_uid:20" "$home"
groups=$(id -G "$account")
[[ " $groups " != *' 80 '* ]]
printf 'created\n' > "$record/state"
id "$account"
echo "Created temporary account; cleanup manifest: $record"
