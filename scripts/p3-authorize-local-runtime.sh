#!/bin/bash
# 仅给本机既有 ccrun 追加本轮公钥；不重建账号或更改其主组/已有权限。
set -euo pipefail
export PATH=/usr/bin:/bin:/usr/sbin:/sbin
umask 077
[[ $# == 1 && $1 == --apply ]] || { echo 'usage: p3-authorize-local-runtime.sh --apply' >&2; exit 2; }
[[ $EUID == 0 && ${SUDO_USER:-} == bing ]] || { echo 'Run with sudo in the local bing terminal' >&2; exit 1; }
home=/Users/ccrun
record=/var/db/ccnm-p3-local-20260908
[[ $(id -u ccrun) == 504 ]]
[[ $(dscl . -read /Users/ccrun NFSHomeDirectory) == "NFSHomeDirectory: $home" ]]
[[ ! -L $home && $(stat -f '%u:%Lp' "$home") == 504:700 ]]
for path in "$home/.ssh" "$home/.ssh/authorized_keys"; do
    [[ ! -L $path ]]
    if [[ -e $path ]]; then
        [[ $(stat -f %u "$path") == 504 ]]
        if [[ $path == "$home/.ssh" ]]; then
            [[ -d $path && $(stat -f %Lp "$path") == 700 ]]
        else
            [[ -f $path && $(stat -f %Lp "$path") == 600 ]]
        fi
    fi
done
key=$(cat /tmp/ccnm-p3-agent-m8wmi9ng/local-runtime.pub)
[[ $key != *$'\n'* && $key == 'ssh-ed25519 '* ]]
fingerprint=$(printf '%s\n' "$key" | ssh-keygen -lf /dev/stdin -E sha256 | awk '{print $2}')
[[ $fingerprint == SHA256:R/ybKNAaN8JQxYETPhpbfeYmh199zOunKmNFT8mP54M ]]

# 清单和备份先于追加；备份仅含公开 authorized_keys，不读取私钥。
mkdir -m 700 "$record"
if [[ -d $home/.ssh ]]; then
    printf 'existing\n' > "$record/ssh-directory"
else
    printf 'created\n' > "$record/ssh-directory"
    mkdir -m 700 "$home/.ssh"
    chown "504:$(id -g ccrun)" "$home/.ssh"
fi
if [[ -f $home/.ssh/authorized_keys ]]; then
    cp "$home/.ssh/authorized_keys" "$record/authorized_keys.before"
else
    printf 'created\n' > "$record/authorized-keys-created"
fi
printf '\nno-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc %s\n' "$key" > "$record/appended-line"
cat "$record/appended-line" >> "$home/.ssh/authorized_keys"
chmod 600 "$home/.ssh/authorized_keys"
chown "504:$(id -g ccrun)" "$home/.ssh/authorized_keys"
echo "Temporary public key appended; existing account preserved. Cleanup record: $record"
