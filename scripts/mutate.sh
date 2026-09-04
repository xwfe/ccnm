#!/usr/bin/env bash
# Break one guard at a time and require a test to notice.
#
#   scripts/mutate.sh
#
# A green test suite says the code passes its tests. It does not say the
# tests would catch the code being wrong. This does: each case below
# removes exactly one guard -- an `if` that refuses something, a flag that
# has to be there, a cleanup that has to happen -- and the run is only
# honest if every one of them turns a test red.
#
#   RED     the guard is tested. What was expected.
#   GREEN   nothing noticed. Either write the missing test or, if the
#           mutation genuinely changes no observable behaviour, say so and
#           delete the case; an "equivalent mutant" is not a failure to
#           fix, but it must not be quietly counted as a pass either.
#
# Cases are written against the code as it is now, so they go stale as it
# moves. A case that prints COULD NOT APPLY has to be rewritten or dropped
# -- it is proving nothing. The set below is the newest round (the write
# path and the interruption paths); earlier rounds were run the same way
# and are recorded in the design doc rather than kept here forever.
#
# Needs a clean tree: every restore is `git checkout <file>`.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  echo "tree not clean; commit first (restores use git checkout)" >&2
  exit 2
fi

red=0
green=0
broken=0

mutate() {
  local name="$1" file="$2" old="$3" new="$4"
  python3 - "$file" "$old" "$new" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
if old not in text:
    sys.exit(2)
open(path, "w").write(text.replace(old, new, 1))
PY
  if [ $? -eq 2 ]; then
    echo "COULD NOT APPLY  $name  (the code moved; rewrite or drop this case)"
    broken=$((broken + 1))
    git checkout -q "$file"
    return
  fi
  local out
  out=$(cargo test --workspace 2>&1)
  if echo "$out" | grep -q 'could not compile'; then
    echo "DID NOT COMPILE  $name"
    broken=$((broken + 1))
  elif echo "$out" | grep -qE 'test result: FAILED'; then
    echo "RED    $name"
    echo "       caught by: $(echo "$out" | grep -E '^test .+ \.\.\. FAILED' | sed 's/ \.\.\. FAILED//;s/^test //' | tr '\n' ' ')"
    red=$((red + 1))
  else
    echo "GREEN  $name   <-- test gap"
    green=$((green + 1))
  fi
  git checkout -q "$file"
}

P=crates/ccnm-core/src/mcp/patch.rs
L=crates/ccnm-core/src/mcp/list.rs
W=crates/ccnm-core/src/work.rs
S=crates/ccnm-core/src/ssh.rs
K=crates/ccnm-core/src/controller.rs
T=crates/ccnm-core/src/tmux.rs
N=crates/ccnm-core/src/launcher.rs
E=crates/ccnm-core/src/session.rs
C=crates/ccnm-cli/src/main.rs

# --- the write path ---------------------------------------------------

mutate "two files may not share a new directory" "$P" \
  '            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}' \
  '            Err(e) if false => { let _ = e; }'

mutate "leftover temp files are never swept" "$P" \
  '    sweep_stale_temps(&plan);
' \
  ''

mutate "the sweep takes temps another patch is using" "$P" \
  '                .map(|t| t.elapsed().is_ok_and(|age| age > STALE_TEMP))
                .unwrap_or(false);' \
  '                .map(|_| true)
                .unwrap_or(false);'

mutate "patch temps are listed like project files" "$L" \
  '            .any(|s| s.starts_with(crate::mcp::patch::TEMP_PREFIX))' \
  '            .any(|s| s.starts_with("no-such-prefix-"))'

# --- sessions and their interruptions ---------------------------------

mutate "interactive sessions bypass tmux" "$K" \
  '    if !spec.mode.is_interactive() {' \
  '    if true {'

mutate "a second session for the same workspace is allowed" "$K" \
  '    if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {' \
  '    if false {'

mutate "the default tmux server instead of ccnm's own" "$T" \
  'Cmd::new(&self.bin).args(["-L", SOCKET]).timeout(TIMEOUT)' \
  'Cmd::new(&self.bin).timeout(TIMEOUT)'

mutate "a live session is started over instead of reported" "$W" \
  '    if tools.runner.run(&tmux.has_session_cmd(&name))?.success() {' \
  '    if false {'

mutate "the transport is assumed to be up" "$W" \
  '    Some(out.stdout_lossy().contains(&payload))' \
  '    Some(true)'

mutate "an unknown transport is reported as down" "$W" \
  '    let payload = transport_payload(dir)?;' \
  '    let Some(payload) = transport_payload(dir) else {
        return Some(false);
    };'

mutate "result picks an interactive session" "$W" \
  '        if spec.workspace != workspace || spec.mode.is_interactive() {' \
  '        if spec.workspace != workspace {'

mutate "result picks by read order, not by time" "$W" \
  '        if best.as_ref().is_none_or(|(_, _, best)| started > *best) {' \
  '        if best.is_none() {'

mutate "attach without a terminal" "$S" \
  '            .arg("-t")
            .arg(&self.alias)
            .args(argv))' \
  '            .arg("-T")
            .arg(&self.alias)
            .args(argv))'

mutate "attach without the remote binary" "$S" \
  '        let mut argv: Vec<&str> = vec![self.ccnm_bin.as_str()];
        argv.extend_from_slice(subcommand);' \
  '        let mut argv: Vec<&str> = Vec::new();
        argv.extend_from_slice(subcommand);'

mutate "the transport gives up as fast as a control command" "$S" \
  '            "ServerAliveCountMax=20",' \
  '            "ServerAliveCountMax=3",'

# --- the two directions a session is started from ---------------------
#
# These are the ones per-hop unit tests structurally cannot catch: both
# ends behaving sensibly with the wrong value. Each has to be caught by
# a test that carries one end's real output into the other end's input.

mutate "work is told the alias home came in on" "$N" \
  '        home_alias: resolved.home_alias.to_string(),
        home_ccnm_bin: resolved.runtime.ccnm_bin(),
        claude_config_dir: resolved.work.claude_config_dir.clone(),
        permission_mode: resolved.workspace.claude_permission_mode,
        prompt: prompt.map(str::to_string),' \
  '        home_alias: resolved.work_ssh.to_string(),
        home_ccnm_bin: resolved.work.ccnm_bin(),
        claude_config_dir: resolved.work.claude_config_dir.clone(),
        permission_mode: resolved.workspace.claude_permission_mode,
        prompt: prompt.map(str::to_string),'

mutate "the session is not told a person is watching" "$E" \
  '.with_interactive(matches!(spec.mode, Mode::Interactive { .. })),' \
  '.with_interactive(false),'

mutate "the work machine waits for a terminal that is elsewhere" "$N" \
  '&[ssh.ccnm_bin(), "run", workspace, "--detached"],' \
  '&[ssh.ccnm_bin(), "run", workspace],'

mutate "where home keeps ccnm is ignored" "$N" \
  'let ssh = Ssh::new(home_alias, env.control_dir.clone())?.with_ccnm_bin(home_ccnm_bin);' \
  'let ssh = Ssh::new(home_alias, env.control_dir.clone())?;'

mutate "the work machine never recognises itself" "$C" \
  '    if config.workspace(workspace).is_ok() {
        return None;
    }
    config.home_from_work()' \
  '    if config.workspace(workspace).is_ok() {
        return None;
    }
    let _ = config.home_from_work();
    None'

echo
echo "$red red, $green green, $broken not applied"
git status --porcelain --untracked-files=no
[ "$green" -eq 0 ] && [ "$broken" -eq 0 ]
