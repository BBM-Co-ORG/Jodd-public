#!/bin/sh
# Builds the AI host image locally and checks it with Jodd's setup code,
# including that a recreate keeps the fingerprint, the CLIs and the lock.
# Local only. Needs Docker and network (the official CLI installers download).
# shellcheck disable=SC1091,SC2154  # lib.sh sets $repo
set -eu
. "$(dirname "$0")/lib.sh"
name=jodd-ai-e2e-image port=2224 pw=e2e-image-pw
state=/var/lib/jodd-ai-host
cleanup() { docker rm -f "$name" >/dev/null 2>&1 || :; docker volume rm -f "$name-home" "$name-state" >/dev/null 2>&1 || :; }
trap cleanup EXIT; cleanup
docker build -q -t jodd-ai-host:e2e -f "$repo/ai-host/docker/Dockerfile" "$repo/ai-host" >/dev/null
# run [extra docker args]: --network none cannot publish a port, so it is added only without it.
run() {
  case " $* " in *' --network none '*) pub= ;; *) pub="-p 127.0.0.1:$port:22" ;; esac
  # The password goes in by inheritance (-e NAME), so it is neither on a command line nor echoed;
  # unset JODD_AI_PASSWORD (pw empty) means the image generates one.
  if [ -n "$pw" ]; then export JODD_AI_PASSWORD="$pw"; else unset JODD_AI_PASSWORD; fi
  # shellcheck disable=SC2086
  docker run -d --name "$name" --init $pub -e JODD_AI_PASSWORD -e JODD_AI_PORT="$port" \
    -v "$name-home:/home/jodd-ai" -v "$name-state:$state" "$@" jodd-ai-host:e2e >/dev/null
}
fail() { echo "FAIL: $*"; docker logs "$name" 2>&1 | tail -n 40; exit 1; }
wait_log() { # $1 pattern, $2 tries (2 s each)
  i=0; until docker logs "$name" 2>&1 | grep -Eq "$1"; do i=$((i+1)); [ "$i" -gt "$2" ] && fail "timed out waiting for log line: $1"; sleep 2; done
}
# What sshd applies to the AI account: `sshd -T -C user=…` evaluates the config files on disk
# (Include + Match) for that connection. The reload itself is checked separately (SIGHUP in the log).
pw_auth() { docker exec "$name" sh -c '/usr/sbin/sshd -T -C user=jodd-ai,host=localhost,addr=127.0.0.1 2>/dev/null | grep -i "^passwordauthentication"'; }
shadow() { docker exec "$name" sh -c "grep '^jodd-ai:' /etc/shadow | cut -d: -f2"; }
sighups() { docker logs "$name" 2>&1 | grep -c 'Received SIGHUP' || :; }

# 1. First start with no network: sshd must still come up and say how to retry.
echo "== 1. first start, no network"
run --network none
wait_log 'jodd-ai-host ready' 30
wait_log 'could not install the CLIs' 60
docker logs "$name" 2>&1 | grep -q 'jodd-ai-host update-clis' || fail "no retry hint without network"
docker exec "$name" sh -c 'pgrep -x sshd >/dev/null' || fail "sshd is not running without network"
echo "ok: sshd up, retry hint printed"
docker rm -f "$name" >/dev/null

# 2. Normal start: CLIs install on first start.
echo "== 2. normal start, CLIs install"
run
wait_log 'jodd-ai-host ready' 30
wait_log 'CLIs installed|could not install the CLIs' 300
docker logs "$name" 2>&1 | grep -q 'CLIs installed' || fail "the CLIs did not install"
docker exec "$name" jodd-ai-host status | grep -q '^codex: */' || fail "codex not found by status"
echo "-- where the official installers put the CLIs:"
docker exec "$name" sh -c 'ls -l /home/jodd-ai/.local/bin 2>&1; ls -d /home/jodd-ai/.codex* 2>&1; su -l jodd-ai -c "command -v claude; command -v codex; echo PATH=\$PATH"'
docker exec "$name" jodd-ai-host status
fp1=$(docker exec "$name" jodd-ai-host fingerprint)
e2e_run_jodd_tests "$port" "$name" "$pw"

# 3. Lock: the running sshd must change (HUP reload), and the symlink must still point at the state file.
echo "== 3. lock / unlock against the running sshd"
[ "$(pw_auth)" = 'passwordauthentication yes' ] || fail "unlocked: sshd -T says '$(pw_auth)'"
h0=$(sighups)
# An open ssh session must survive the reload (before OpenSSH 9.8 `pkill -HUP -x sshd` kills them all).
e2e_session_start "$port" jodd-ai "$pw" || fail "could not open a test ssh session"
docker exec "$name" jodd-ai-host lock
[ "$(pw_auth)" = 'passwordauthentication no' ] || fail "locked: sshd -T says '$(pw_auth)'"
sleep 1; [ "$(sighups)" -gt "$h0" ] || fail "sshd did not log a SIGHUP reload after lock"
[ "$(docker exec "$name" readlink /etc/ssh/sshd_config.d/50-jodd-ai.conf)" = "$state/50-jodd-ai.conf" ] || fail "drop-in symlink moved"
docker exec "$name" grep -q 'PasswordAuthentication no' /etc/ssh/sshd_config.d/50-jodd-ai.conf || fail "symlink does not show the locked state"
e2e_session_ping alive || { e2e_session_end; fail "an open ssh session died when lock reloaded sshd"; }
e2e_session_end
echo "ok: locked (sshd -T: $(pw_auth)), the open ssh session survived, SIGHUP logged, symlink -> $state/50-jodd-ai.conf"

# 4. Recreate (same volumes): same fingerprint, CLIs still there, no reinstall, still locked.
echo "== 4. recreate"
docker rm -f "$name" >/dev/null; run
e2e_wait_port "$port" || fail "sshd did not come back after recreate"
fp2=$(docker exec "$name" jodd-ai-host fingerprint)
[ "$fp1" = "$fp2" ] || fail "fingerprint changed: $fp1 -> $fp2"
docker exec "$name" jodd-ai-host status | grep -q '^claude: */' || fail "claude lost on recreate"
docker exec "$name" jodd-ai-host status | grep -q '^codex: */' || fail "codex lost on recreate"
docker logs "$name" 2>&1 | grep -q 'installing claude and codex' && fail "recreate reinstalled the CLIs"
docker exec "$name" jodd-ai-host status | grep -q '^password: *off' || fail "lock did not survive the recreate"
[ "$(pw_auth)" = 'passwordauthentication no' ] || fail "locked after recreate: sshd -T says '$(pw_auth)'"
docker exec "$name" jodd-ai-host unlock
[ "$(pw_auth)" = 'passwordauthentication yes' ] || fail "unlocked: sshd -T says '$(pw_auth)'"
[ "$(docker exec "$name" readlink /etc/ssh/sshd_config.d/50-jodd-ai.conf)" = "$state/50-jodd-ai.conf" ] || fail "drop-in symlink moved after unlock"
echo "ok: fingerprint $fp2 kept, CLIs kept, lock survived, unlock works (sshd -T: $(pw_auth))"

# 5. A password changed inside the container survives a restart; a deliberate env change is applied.
echo "== 5. password survives restart"
newpw=e2e-image-changed-pw
docker exec "$name" sh -c "printf 'jodd-ai:%s\n' '$newpw' | chpasswd"
sh1=$(shadow)
docker restart "$name" >/dev/null
e2e_wait_port "$port" || fail "sshd did not come back after restart"
[ "$(shadow)" = "$sh1" ] || fail "restart changed the password hash (a changed password was reverted)"
docker exec "$name" sh -c 'test ! -e /var/lib/jodd-ai-host/initial-password' || fail "plain-text initial-password is in the state volume"
e2e_run_jodd_tests "$port" "$name" "$newpw"
pw=e2e-image-env-changed-pw
docker rm -f "$name" >/dev/null; run
e2e_wait_port "$port" || fail "sshd did not come back after recreate with a new env password"
e2e_run_jodd_tests "$port" "$name" "$pw"
echo "ok: restart kept the changed password; a new JODD_AI_PASSWORD is applied"

# 6. No JODD_AI_PASSWORD: a password is generated once, shown once, and not stored in plain text.
echo "== 6. generated password"
cleanup; pw=
run
wait_log 'jodd-ai-host ready' 30
wait_log 'CLIs installed|could not install the CLIs' 300
gen=$(docker logs "$name" 2>&1 | sed -n 's/.*generated a password for jodd-ai: \([^ ]*\) .*/\1/p')
[ -n "$gen" ] || fail "no generated password in the log"
docker exec "$name" sh -c 'test ! -e /var/lib/jodd-ai-host/initial-password' || fail "plain-text initial-password is in the state volume"
e2e_run_jodd_tests "$port" "$name" "$gen"
sh1=$(shadow)
docker restart "$name" >/dev/null
e2e_wait_port "$port" || fail "sshd did not come back after restart"
[ "$(shadow)" = "$sh1" ] || fail "restart regenerated the password"
[ "$(docker logs "$name" 2>&1 | grep -c 'generated a password')" = 1 ] || fail "the generated password was shown more than once"
echo "ok: generated password works, kept across restart, shown once"

# 7. A line break in JODD_AI_PASSWORD is refused with a message; sshd still starts.
echo "== 7. password with a line break"
cleanup; pw=$(printf 'line1\nline2')
run --network none
wait_log 'jodd-ai-host ready' 30
docker logs "$name" 2>&1 | grep -q 'contains a line break' || fail "no line-break message"
docker exec "$name" sh -c 'pgrep -x sshd >/dev/null' || fail "sshd is not running"
echo "ok: refused with a message, sshd up"

# 8. A broken drop-in stops the container with a message that names the file.
echo "== 8. broken drop-in"
docker rm -f "$name" >/dev/null
docker run --rm --entrypoint sh -v "$name-state:$state" jodd-ai-host:e2e -c "echo 'NotAnSshdKeyword yes' > $state/50-jodd-ai.conf"
run --network none
i=0; while [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ]; do i=$((i+1)); [ "$i" -gt 30 ] && fail "container kept running with a broken drop-in"; sleep 1; done
[ "$(docker inspect -f '{{.State.ExitCode}}' "$name")" != 0 ] || fail "exit code 0 with a broken drop-in"
docker logs "$name" 2>&1 | grep -q "$state/50-jodd-ai.conf" || fail "message does not name the drop-in"
echo "ok: stopped with a message naming $state/50-jodd-ai.conf"
# 9. The entrypoint repairs the account's login PATH as the account: a ~/.profile that is a link to a
#    root-owned file (planted by the account) is not written through.
echo "== 9. link in the account's home"
cleanup; pw=e2e-image-pw
docker run --rm --entrypoint sh -v "$name-home:/home/jodd-ai" -v "$name-state:$state" jodd-ai-host:e2e -c \
  "printf 'orig\n' > $state/e2e-target && chmod 600 $state/e2e-target && ln -sf $state/e2e-target /home/jodd-ai/.profile"
run --network none
wait_log 'jodd-ai-host ready' 30
docker logs "$name" 2>&1 | grep -q 'could not add ~/.local/bin' || fail "no warning about the linked ~/.profile"
[ "$(docker exec "$name" sh -c "cat $state/e2e-target; stat -c '%U %a' $state/e2e-target")" = "orig
root 600" ] || fail "the entrypoint wrote through the link in the account's home (target changed, or its owner/mode)"
echo "ok: root did not write through a link in the account's home"
echo "image e2e: OK"
