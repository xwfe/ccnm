#!/bin/bash
# P7.3 第一步：把本轮公钥装进本机既有 ccrun，并建立本轮的 root 清单。
#
# 只追加一行 authorized_keys，不重建账号、不改 UID/shell/密码/主组。清单先于
# 变更写入，后面两步（准入、主组）都依赖它存在——所以这个脚本要先跑。
#
# 公钥直接写在下面，不从 /tmp 之类的路径读：外部文件多一次被换掉的机会，而
# 内嵌的字面量没有这个窗口。指纹仍然当场重算并比对，用来挡住"改了 key 忘了
# 改指纹"这种手滑。
#
# 私钥在 fodelf，本机从头到尾看不到它。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
case "${1:-}" in
    --check|--apply) action=$1 ;;
    *) echo 'usage: p7-authorize-local-runtime.sh --check|--apply' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Darwin ]] || { echo 'macOS required' >&2; exit 1; }
# 两个动作都要 root：清单是 root 所有的 0700，连核对都读不到。--check 仍然只读。
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || {
    echo 'Run with sudo in the local bing terminal (--check needs it only to read the root record);' >&2
    echo 'do not send the password to the agent' >&2
    exit 1; }

home=/Users/ccrun
record=/var/db/ccnm-p7-local-20260910
keys=$home/.ssh/authorized_keys
key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJVOyhn8RhtGDxPelIiwG+1WrrVytAzP31DdMcmu40tC ccnm-p7-runtime'
fingerprint=SHA256:dX703DgmAEHimpXj1pxch7U7vFMfrFnjVTdPuyidWNc
options=no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc

[[ $(id -u ccrun) == 504 ]] || { echo 'ccrun is not uid 504; refusing to guess' >&2; exit 1; }
[[ $(dscl . -read /Users/ccrun NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
[[ ! -L $home && $(stat -f '%u:%Lp' "$home") == 504:700 ]]
for path in "$home/.ssh" "$keys"; do
    [[ ! -L $path ]] || { echo "$path is a symlink; stopping" >&2; exit 1; }
    if [[ -e $path ]]; then
        [[ $(stat -f %u "$path") == 504 ]]
        if [[ $path == "$home/.ssh" ]]; then
            [[ -d $path && $(stat -f %Lp "$path") == 700 ]]
        else
            [[ -f $path && $(stat -f %Lp "$path") == 600 ]]
        fi
    fi
done

[[ $key != *$'\n'* && $key == 'ssh-ed25519 '* ]]
actual=$(printf '%s\n' "$key" | ssh-keygen -lf /dev/stdin -E sha256 | awk '{print $2}')
[[ $actual == "$fingerprint" ]] || {
    echo "Key fingerprint is $actual, not the pinned $fingerprint; refusing to install it" >&2
    exit 1; }

installed=no
if [[ -f $keys ]] && grep -qF "$key" "$keys"; then
    installed=yes
fi
existing_lines=0
[[ -f $keys ]] && existing_lines=$(grep -c . "$keys" || true)

if [[ $action == --check ]]; then
    printf 'account=ccrun uid=504 home=%s\n' "$home"
    printf 'authorized_keys: %s, %s non-empty line(s)\n' \
        "$([[ -f $keys ]] && echo present || echo absent)" "$existing_lines"
    printf 'this round key already installed=%s\n' "$installed"
    printf 'record %s: %s\n' "$record" "$([[ -d $record ]] && echo present || echo absent)"
    if [[ $installed == yes ]]; then
        echo 'Nothing to do; apply would refuse rather than append a duplicate.'
    else
        printf 'apply would append one line: %s <this round key>\n' "$options"
    fi
    exit 0
fi

[[ $installed == no ]] || {
    echo 'This key is already in authorized_keys; refusing to append a duplicate' >&2; exit 1; }

# 清单先于变更写入；重跑时目录已在就复用，但属性必须对得上。
if [[ -e $record ]]; then
    [[ ! -L $record && -d $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]] || {
        echo "$record exists but is not a root-owned 0700 directory; review it first" >&2; exit 1; }
else
    mkdir -m 700 "$record"
fi

if [[ -d $home/.ssh ]]; then
    printf 'existing\n' > "$record/ssh-directory"
else
    printf 'created\n' > "$record/ssh-directory"
    mkdir -m 700 "$home/.ssh"
    chown "504:$(id -g ccrun)" "$home/.ssh"
fi
if [[ -f $keys ]]; then
    cp "$keys" "$record/authorized_keys.before"
else
    printf 'created\n' > "$record/authorized-keys-created"
fi
# 记下将要追加的那几个字节，撤销时按它精确删除，不整份恢复备份。
printf '\n%s %s\n' "$options" "$key" > "$record/appended-line"
cat "$record/appended-line" >> "$keys"
chmod 600 "$keys"
chown "504:$(id -g ccrun)" "$keys"
echo "Temporary public key appended; the existing account is untouched. Record: $record"
echo 'Next: p7-grant-local-ssh-access.sh --check'
