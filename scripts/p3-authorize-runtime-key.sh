#!/bin/bash
# 本轮 fodelf 临时账号专用；只安装已核对指纹的公钥，不接触 Agent 凭据。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
[[ $# == 1 && $1 == --apply ]] || { echo 'usage: p3-authorize-runtime-key.sh --apply' >&2; exit 2; }
[[ $EUID == 0 && ${SUDO_USER:-} == fodelf ]] || { echo 'Run with sudo in the fodelf terminal' >&2; exit 1; }

home=/Users/ccnmp3test
record=/var/db/ccnm-p3-account-20260908
[[ $(id -u ccnmp3test) == 550 ]]
[[ $(dscl . -read /Users/ccnmp3test NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
[[ ! -L $home && $(stat -f '%u:%Lp' "$home") == 550:700 ]]
[[ ! -L $record && $(stat -f '%u:%Lp' "$record") == 0:700 ]]
[[ $(cat "$record/state") == created ]]
[[ ! -e $home/.ssh && ! -L $home/.ssh ]] || { echo 'Existing .ssh; refusing to overwrite' >&2; exit 1; }

key=$(cat /tmp/ccnm-p3-setup.TnaRle/runtime.pub)
[[ $key != *$'\n'* && $key == 'ssh-ed25519 '* ]]
fingerprint=$(printf '%s\n' "$key" | ssh-keygen -lf /dev/stdin -E sha256 | awk '{print $2}')
[[ $fingerprint == SHA256:AlJxpK96ks8KDg0woK1tSluntPXRt9XV0ZA/Rp2HPZ4 ]]

# .ssh 在写完前保持 root 所有，避免未完成的授权提前可用。
printf '%s\n' "$home/.ssh" "$home/.ssh/authorized_keys" > "$record/ssh-resources.txt"
mkdir -m 700 "$home/.ssh"
printf 'no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc %s\n' "$key" > "$home/.ssh/authorized_keys"
chmod 600 "$home/.ssh/authorized_keys"
chown 550:20 "$home/.ssh/authorized_keys" "$home/.ssh"
echo 'Temporary public key installed; forwarding disabled. SSH and isolation still require verification.'
