#!/usr/bin/env bash
# Put this build on both machines, restart the controller, and check the
# pair still works.
#
#   scripts/deploy.sh <other-machine-ssh-alias> [workspace]
#
# Run it on whichever machine has the Rust toolchain; it installs here and
# pushes there. The home machine usually has no toolchain, so in practice
# that means: run it on the work machine with the home alias, or on a
# machine that is neither with both aliases reachable.
#
# Two things it does that are easy to get wrong by hand:
#
#   * It never `cp`s over the binary in place. On Apple Silicon, writing
#     into a Mach-O that has already been executed invalidates its code
#     signature, and every later exec of it dies with SIGKILL (exit 137)
#     while the already-running old process carries on with the old code.
#     The symptom is a `ccnm --version` that is "Killed: 9" and a doctor
#     that reports an empty reply, with launchctl insisting the controller
#     is fine. Writing a new file and renaming it over the old one changes
#     the inode, so the running process keeps its own copy and the next
#     exec gets a whole, correctly signed one.
#
#   * It restarts the controller, because a LaunchAgent goes on running
#     the binary it started with. Sessions survive that restart: the tmux
#     server is in its own process group.
set -euo pipefail
cd "$(dirname "$0")/.."

OTHER=${1:-}
WORKSPACE=${2:-}
if [ -z "$OTHER" ]; then
  echo "usage: scripts/deploy.sh <other-machine-ssh-alias> [workspace]" >&2
  exit 2
fi

BIN=${CCNM_BIN:-$HOME/.local/bin/ccnm}
REMOTE_BIN=${CCNM_REMOTE_BIN:-.local/bin/ccnm}

echo "==> building"
cargo build --release

echo "==> installing here: $BIN"
mkdir -p "$(dirname "$BIN")"
cp target/release/ccnm "$BIN.new"
mv "$BIN.new" "$BIN"

echo "==> installing on $OTHER: ~/$REMOTE_BIN"
scp -q target/release/ccnm "$OTHER:$REMOTE_BIN.new"
ssh "$OTHER" "mkdir -p \$(dirname ~/$REMOTE_BIN) && mv ~/$REMOTE_BIN.new ~/$REMOTE_BIN"

# The controller lives on the work machine. Whichever of the two this is,
# restart the one that exists rather than making the caller say which.
PLIST="$HOME/Library/LaunchAgents/dev.ccnm.work-controller.plist"
if [ -f "$PLIST" ]; then
  echo "==> restarting the controller here"
  "$BIN" work-controller install | tail -2
elif ssh "$OTHER" "test -f ~/Library/LaunchAgents/dev.ccnm.work-controller.plist"; then
  echo "==> restarting the controller on $OTHER"
  ssh "$OTHER" "~/$REMOTE_BIN work-controller install" | tail -2
else
  echo "==> no controller installed on either machine; skipping the restart"
fi

echo "==> versions"
"$BIN" --version
ssh "$OTHER" "~/$REMOTE_BIN --version"

if [ -n "$WORKSPACE" ]; then
  echo "==> doctor $WORKSPACE"
  # Config lives on the home machine, so doctor runs wherever that is.
  if [ -f "$HOME/.config/ccnm/config.toml" ]; then
    "$BIN" doctor "$WORKSPACE" || true
  else
    ssh "$OTHER" "~/$REMOTE_BIN doctor $WORKSPACE" || true
  fi
fi
