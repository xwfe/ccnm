#!/bin/bash
# P12 第一步：在 Debian Runtime Node 上准备专用执行身份。要 root。
#
# 它只做三件需要特权的事，别的都交给无特权的
# p12-runtime-toolchain.sh（那一个以 ccrun 自己的身份跑）：
#
#   1. 建一个专用账号 ccrun：没有密码、不在 sudo/docker/adm 里、home 0700；
#   2. 把本轮那把一次性公钥写进它的 authorized_keys；
#   3. 装 Rust 链接期要的 C 工具链（gcc/libc6-dev/make）——**这是本机唯一的
#      系统级安装**，因为 rustc 自己不带 linker。
#
# 为什么工具链本体（rustup、Node）不在这里装：它们装在 ccrun 自己 home 里就够
# 了，用 root 装只会把文件属主搞错，并且让"哪些东西是这一轮加的"变得说不清。
#
# 为什么公钥内嵌：外部文件多一次被换掉的机会。指纹当场重算并比对，挡住"换了
# key 忘了改指纹"。私钥在客户端机器上，这台机器从头到尾看不到它。
#
# 清单先于变更写入 /var/lib/ccnm-p12-20260911（root 0700）。--revert 只撤清单
# 里记过的东西：没记"是本轮建的账号"就绝不 userdel，没记"是本轮装的包"就绝不
# purge。别处对 authorized_keys 的并发修改一律保留。
set -euo pipefail
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
umask 022
case "${1:-}" in
    --check|--apply|--revert) action=$1 ;;
    *) echo 'usage: p12-provision-linux-runtime.sh --check|--apply|--revert' >&2; exit 2 ;;
esac
[[ $# == 1 ]] || exit 2
[[ $(uname -s) == Linux && -f /etc/debian_version ]] || {
    echo 'Debian-family Linux required (it uses useradd/apt-get)' >&2; exit 1; }
[[ $EUID == 0 ]] || { echo 'Run as root (sudo) on the Runtime Node' >&2; exit 1; }

user=ccrun
home=/home/$user
record=/var/lib/ccnm-p12-20260911
keys=$home/.ssh/authorized_keys
key='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEaPJu/yn6kpFSIS5CA05LzvnV2isdrJcLKPAdvsoz+O ccnm-p12-runtime'
fingerprint=SHA256:wIXG2Ox+8zC3SbLpif+peAzlskrY93X2WwEDzV38/Ek
options=no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc
# rustc 调 `cc` 做链接，所以没有 C 工具链就没有 Rust 构建。make 不是 cargo 要
# 的，是项目里带 build script 的依赖要的。g++ 故意不装：本轮没有 C++ 目标，装
# 了就得在支持矩阵里多解释一项。
#
# ripgrep 不是项目要的，是 **ccnm 自己**要的：`search_text` 不自己扫文件，它调
# `rg`。Runtime 上没有 rg，七工具里就少一个，报的是"ripgrep is not installed on
# the Runtime Node"。所以它属于 Runtime 的前置条件，和 git 一样。
packages=(gcc libc6-dev make ripgrep)
line_file=$record/appended-line

[[ $key != *$'\n'* && $key == 'ssh-ed25519 '* ]]
actual=$(printf '%s\n' "$key" | ssh-keygen -lf /dev/stdin -E sha256 | awk '{print $2}')
[[ $actual == "$fingerprint" ]] || {
    echo "Key fingerprint is $actual, not the pinned $fingerprint; refusing to install it" >&2
    exit 1; }

account_exists=no
id "$user" >/dev/null 2>&1 && account_exists=yes
record_exists=no
[[ -d $record && ! -L $record ]] && record_exists=yes
ours=no
[[ $record_exists == yes && -f $record/created-account ]] && ours=yes

missing=()
for pkg in "${packages[@]}"; do
    dpkg-query -W -f='${db:Status-Status}\n' "$pkg" 2>/dev/null | grep -qx installed || missing+=("$pkg")
done

key_installed=no
[[ -f $keys ]] && grep -qF "$key" "$keys" && key_installed=yes

# ---------------------------------------------------------------- --check

if [[ $action == --check ]]; then
    printf 'account %s: %s\n' "$user" "$account_exists"
    if [[ $account_exists == yes ]]; then
        printf '  id: %s\n' "$(id "$user")"
        printf '  home: %s\n' "$(stat -c '%U:%G %a' "$home" 2>/dev/null || echo 'missing')"
    fi
    printf 'record %s: %s (created-account recorded: %s)\n' "$record" "$record_exists" "$ours"
    printf 'this round key already installed: %s\n' "$key_installed"
    if ((${#missing[@]})); then
        printf 'packages to install: %s\n' "${missing[*]}"
    else
        printf 'packages: all present already (%s)\n' "${packages[*]}"
    fi
    if [[ $account_exists == yes && $ours == no ]]; then
        echo
        echo 'apply would REFUSE: an account by this name already exists and this round did not'
        echo 'create it. Nothing here guesses whose account it is.'
    fi
    exit 0
fi

# ---------------------------------------------------------------- --apply

if [[ $action == --apply ]]; then
    [[ $account_exists == no || $ours == yes ]] || {
        echo "$user already exists and no record says this round created it; refusing to reuse it" >&2
        exit 1; }
    [[ $key_installed == no ]] || {
        echo 'This key is already in authorized_keys; refusing to append a duplicate' >&2; exit 1; }

    if [[ $record_exists == yes ]]; then
        [[ $(stat -c '%u:%a' "$record") == 0:700 ]] || {
            echo "$record exists but is not a root-owned 0700 directory; review it first" >&2; exit 1; }
    else
        mkdir -m 700 "$record"
    fi

    # 包先装：它是唯一的系统级改动，失败了就不该再建账号。装之前后各拍一次
    # dpkg 名单，差集就是本轮真正新增的包——`apt-get purge <我要的三个>` 会留下
    # 依赖，而 autoremove 会顺手删掉本来就孤立的别人的包。
    if ((${#missing[@]})); then
        dpkg-query -W -f='${binary:Package}\n' > "$record/dpkg-before"
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${missing[@]}"
        dpkg-query -W -f='${binary:Package}\n' > "$record/dpkg-after"
        comm -13 <(sort "$record/dpkg-before") <(sort "$record/dpkg-after") > "$record/packages-installed"
        printf 'installed %s new package(s)\n' "$(grep -c . "$record/packages-installed" || true)"
    else
        : > "$record/packages-installed"
        echo 'packages were already present; none recorded for removal'
    fi

    if [[ $account_exists == no ]]; then
        # --create-home 用 /etc/login.defs 的 HOME_MODE；这台机器是 0700，
        # 但不依赖它，下面自己确认一次。
        useradd --create-home --shell /bin/bash \
            --comment 'ccnm Runtime Executor (P12)' "$user"
        printf '%s\n' "$user" > "$record/created-account"
        # 没有密码：password login 不可能，只剩 authorized_keys 这一条入口。
        passwd --lock "$user" >/dev/null
        account_exists=yes
    fi

    chmod 700 "$home"
    chown "$user:$user" "$home"

    groups_now=$(id -Gn "$user")
    for bad in sudo wheel admin adm docker staff; do
        if [[ " $groups_now " == *" $bad "* ]]; then
            echo "$user is in $bad; that defeats the point of a dedicated Runtime identity" >&2
            exit 1
        fi
    done

    if [[ -d $home/.ssh ]]; then
        printf 'existing\n' > "$record/ssh-directory"
    else
        printf 'created\n' > "$record/ssh-directory"
        mkdir -m 700 "$home/.ssh"
        chown "$user:$user" "$home/.ssh"
    fi
    if [[ -f $keys ]]; then
        cp "$keys" "$record/authorized_keys.before"
    else
        printf 'created\n' > "$record/authorized-keys-created"
    fi
    # 记下将要追加的那几个字节，撤销时按它精确删除，不整份恢复备份。
    printf '%s %s\n' "$options" "$key" > "$line_file"
    cat "$line_file" >> "$keys"
    chmod 600 "$keys"
    chown "$user:$user" "$keys"

    echo
    echo "Done. Record: $record"
    printf 'identity: %s\n' "$(id "$user")"
    printf 'home: %s\n' "$(stat -c '%U:%G %a' "$home")"
    echo 'Next, from the client machine:'
    echo "  ssh -i ~/.ssh/ccnm-p12-20260911 $user@<host> id"
    echo '  then scripts/p12-runtime-toolchain.sh --check  (runs as this account, no root)'
    exit 0
fi

# ---------------------------------------------------------------- --revert

[[ $record_exists == yes ]] || { echo "Missing this round's record $record" >&2; exit 1; }
[[ $(stat -c '%u:%a' "$record") == 0:700 ]]

# Every `appended-line*` in the record, not just the one --apply wrote: a
# round with a second Host (its own key, its own private half) records its
# line next to the first, and the revert has to take both back out.
for line_record in "$record"/appended-line*; do
    [[ -f $line_record ]] || continue
    [[ -f $keys && ! -L $keys ]] || continue
    appended=$(grep -v '^$' "$line_record")
    if [[ -n $appended ]] && grep -qxF "$appended" "$keys"; then
        tmp=$(mktemp "$home/.ssh/.authorized_keys.p12.XXXXXX")
        trap 'rm -f "$tmp"' EXIT
        grep -vxF "$appended" "$keys" > "$tmp" || true
        chmod 600 "$tmp"
        chown "$user:$user" "$tmp"
        mv "$tmp" "$keys"
        trap - EXIT
        ! grep -qxF "$appended" "$keys"
        printf 'Removed the key line recorded in %s; every other line kept.\n' \
            "$(basename "$line_record")"
    else
        printf 'The line in %s was not present; nothing removed.\n' "$(basename "$line_record")"
    fi
done
if [[ -f $record/authorized-keys-created && -f $keys && ! -s $keys ]]; then
    rm "$keys"
    echo 'authorized_keys was created by this round and is now empty; removed.'
fi

if [[ -f $record/created-account ]]; then
    # userdel 会连 home 一起删，工具链和项目副本都在里面——这是本轮的设计，
    # 不是意外。有进程还在跑时 userdel 会失败，如实报出来让人先停掉它。
    if pgrep -u "$user" >/dev/null 2>&1; then
        echo "Processes are still running as $user; stop them first:" >&2
        pgrep -a -u "$user" >&2 || true
        exit 1
    fi
    userdel --remove "$user"
    echo "Account $user and its home removed."
    rm -f "$record/created-account"
else
    echo "No record says this round created $user; leaving the account alone."
fi

if [[ -s $record/packages-installed ]]; then
    mapfile -t installed < "$record/packages-installed"
    DEBIAN_FRONTEND=noninteractive apt-get purge -y "${installed[@]}"
    printf 'purged %s package(s) this round had installed.\n' "${#installed[@]}"
else
    echo 'No packages recorded as installed by this round.'
fi

rm -f "$record"/*
rmdir "$record"
echo "Record $record removed."
echo "Verify: id $user; dpkg-query -W gcc make libc6-dev"
