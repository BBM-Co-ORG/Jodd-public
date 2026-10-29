#!/bin/sh
# Runs ai-host/install/linux.sh in clean containers, then checks the result with
# Jodd's real setup code. Local only; needs Docker and network (packages and the
# official CLI installers are downloaded for real).
# Usage: scripts/ai-host-e2e/linux.sh [image ...]   (default: the four below)
#
# Per image, three containers (a fourth on ubuntu:24.04):
#   hardened  a server that already says `PasswordAuthentication no` globally and has an
#             admin `Match` block (seeded through the installer's TEST-ONLY knob
#             JODD_AI_SEED_CONFIG, which appends a file to sshd_config after the packages
#             are installed). Full install with the real CLIs, then Jodd's own Setup,
#             lock/unlock/revoke, idempotence, password and lock rules.
#   swallow   sshd preinstalled, and a sshd_config whose `Match` block comes BEFORE its
#             `Include sshd_config.d/*.conf` - the Include would be swallowed by that
#             Match - so the installer must put its Include first.
#   rollback  a sshd_config sshd rejects: the installer must exit 1 with sshd's message
#             and leave /etc/ssh byte for byte as it was (fresh, and with a drop-in
#             already installed).
#   authgate  (ubuntu:24.04 only) a global `AuthenticationMethods publickey` and an AllowUsers
#             without jodd-ai: the installer warns naming both and changes neither; doctor FIXes.
# JODD_AI_START_SSHD=1 (also test-only) starts /usr/sbin/sshd directly, since the
# containers have no init system.
# shellcheck disable=SC1091,SC2154,SC2016  # lib.sh sets $repo; the single-quoted scripts are for the container's shell
set -eu
. "$(dirname "$0")/lib.sh"
images=${*:-ubuntu:24.04 debian:12 fedora:latest alpine:latest}
port=2225 pw=e2e-linux-pw
name=
cleanup() { [ -z "$name" ] || docker rm -f "$name" >/dev/null 2>&1 || :; }
trap cleanup EXIT

fail() { echo "FAIL [$img/$scenario]: $*"; exit 1; }
start() { # $1 scenario
  scenario=$1
  cleanup
  name="jodd-ai-e2e-$(echo "$img" | tr ':/.' '---')-$scenario"
  docker rm -f "$name" >/dev/null 2>&1 || :
  docker run -d --name "$name" --init -p "127.0.0.1:$port:22" -v "$repo/ai-host:/kit:ro" "$img" sleep infinity >/dev/null
}
x() { docker exec "$name" sh -c "$1"; }
install() { # extra docker-exec args first, e.g. -e JODD_AI_SKIP_CLIS=1
  docker exec -e JODD_AI_START_SSHD=1 "$@" "$name" sh /kit/install/linux.sh
}
# OpenSSH preinstalled, as on a server that already runs sshd.
prep_sshd() {
  x 'if command -v apt-get >/dev/null; then export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq openssh-server >/dev/null
     elif command -v dnf >/dev/null; then dnf -y -q install openssh-server >/dev/null
     else apk add -q --no-cache openssh >/dev/null; fi
     ssh-keygen -A >/dev/null; mkdir -p /run/sshd'
}
# A distro's own drop-ins (Fedora's crypto policy) are only valid at the top level, so with the
# admin's Match above the Include they would make sshd reject the seed config before the
# installer ran. Empty them: the scenarios simulate a hand-written config.
empty_dropins() { x 'mkdir -p /etc/ssh/sshd_config.d; for f in /etc/ssh/sshd_config.d/*.conf; do [ -f "$f" ] && : > "$f"; done; :'; }
# A real password login from the host, through the ssh client's askpass. Succeeds only if
# the server accepted the password. The password goes by environment, not argv.
ssh_pw() { # $1 user $2 password
  ap=$(mktemp); printf '#!/bin/sh\nprintf "%%s\\n" "$E2E_PW"\n' > "$ap"; chmod 700 "$ap"
  rc=0
  E2E_PW=$2 SSH_ASKPASS=$ap SSH_ASKPASS_REQUIRE=force DISPLAY=none ssh -o PubkeyAuthentication=no \
    -o PreferredAuthentications=password,keyboard-interactive -o NumberOfPasswordPrompts=1 \
    -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 \
    -p "$port" "$1@127.0.0.1" true </dev/null >/dev/null 2>&1 || rc=$?
  rm -f "$ap"; return $rc
}
# sshd_config and everything in sshd_config.d: names, modes, owners, content. (Not the rest
# of /etc/ssh: the installer's package step may add the ssh client's files there.)
snap() {
  x 'find /etc/ssh/sshd_config /etc/ssh/sshd_config.d | sort | while read -r f; do
       if [ -f "$f" ]; then echo "$f $(sha256sum < "$f") $(stat -c "%a %U:%G" "$f")"; else echo "$f dir $(stat -c "%a %U:%G" "$f")"; fi
     done'
}
shadow() { x "grep '^jodd-ai:' /etc/shadow | cut -d: -f2"; }
make_user() { # $1 name, $2 password: an ordinary other account
  x "if command -v useradd >/dev/null; then useradd -m $1; else adduser -D $1; fi; printf '%s:%s\n' $1 $2 | chpasswd"
}

for img in $images; do
  # ============================== hardened ==============================
  start hardened
  # Review Focus 1: password login off globally, plus an admin Match block.
  x 'printf "PasswordAuthentication no\nMatch User someoneelse\n    X11Forwarding no\n" > /etc/ssh.seed'
  install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SEED_CONFIG=/etc/ssh.seed
  e2e_wait_port "$port" || fail "sshd is not reachable on $port"
  x '/usr/sbin/sshd -t' || fail "sshd -t rejects the configuration"
  # the admin's global setting still applies to other accounts
  x '/usr/sbin/sshd -T -C user=root,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication no"' \
    || fail "root's password setting changed"
  x '/usr/sbin/sshd -T -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication yes"' \
    || fail "jodd-ai cannot use a password"
  make_user someoneelse "$pw"
  ssh_pw jodd-ai "$pw" || fail "password login failed for jodd-ai"
  ssh_pw someoneelse "$pw" && fail "password login WORKS for another account"
  ssh_pw jodd-ai wrong-password && fail "a wrong password was accepted"
  echo "ok [$img]: password login works for jodd-ai only (someoneelse and root refused)"
  d=$(x 'jodd-ai-host doctor') || { echo "$d"; fail "doctor reports a problem"; }
  echo "$d"
  # both CLIs must be on the plain login shell's PATH (`-lc`), not only on the interactive one (`-lic`)
  for c in claude codex; do
    printf '%s\n' "$d" | grep -q "$c found (login shell -lc)" || fail "$c is not found by the login shell (-lc)"
  done
  st=$(x 'jodd-ai-host status'); echo "$st"
  for c in claude codex; do
    printf '%s\n' "$st" | grep -Eq "^$c: +/[^ ]+ \([^)]+\)" || fail "status shows no version for $c: the helper's own login shell does not find it"
  done
  sum1=$(snap)
  e2e_run_jodd_tests "$port" "$name" "$pw" || fail "Jodd's setup tests failed"

  # idempotent: a second run changes no sshd config, and not the password
  sh1=$(shadow)
  install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 >/dev/null
  [ "$sum1" = "$(snap)" ] || fail "re-run changed the sshd configuration"
  [ "$sh1" = "$(shadow)" ] || fail "re-run changed the password hash"
  # a password the admin changed is not reset by a re-run with the same JODD_AI_PASSWORD, nor by one with none
  x "printf 'jodd-ai:%s\n' admin-changed-pw | chpasswd"
  sh2=$(shadow)
  install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 >/dev/null
  [ "$sh2" = "$(shadow)" ] || fail "re-run reset a password the admin changed"
  install -e JODD_AI_SKIP_CLIS=1 >/dev/null
  [ "$sh2" = "$(shadow)" ] || fail "re-run without JODD_AI_PASSWORD reset the password"
  ssh_pw jodd-ai admin-changed-pw || fail "the admin's password no longer works"
  # a deliberately changed JODD_AI_PASSWORD is applied
  install -e JODD_AI_PASSWORD=e2e-linux-new-pw -e JODD_AI_SKIP_CLIS=1 >/dev/null
  ssh_pw jodd-ai e2e-linux-new-pw || fail "a new JODD_AI_PASSWORD was not applied"
  ssh_pw jodd-ai admin-changed-pw && fail "the old password still works"
  [ "$sum1" = "$(snap)" ] || fail "password changes touched the sshd configuration"
  echo "ok [$img]: re-run changes no sshd config; passwords: unchanged unless JODD_AI_PASSWORD changed"

  # a lock set by the admin survives a re-run
  x 'jodd-ai-host lock' >/dev/null
  sum_locked=$(snap)
  install -e JODD_AI_SKIP_CLIS=1 >/dev/null
  [ "$sum_locked" = "$(snap)" ] || fail "re-run changed the sshd configuration while locked"
  x '/usr/sbin/sshd -T -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication no"' \
    || fail "re-run unlocked the account"
  ssh_pw jodd-ai e2e-linux-new-pw && fail "password login worked while locked"
  x 'jodd-ai-host unlock' >/dev/null
  echo "ok [$img]: a lock survives a re-run"

  # An open ssh session must survive every sshd reload: lock, unlock, the installer's re-run
  # (before OpenSSH 9.8 a `pkill -HUP -x sshd` kills them all, the admin's own included).
  e2e_session_start "$port" jodd-ai e2e-linux-new-pw || fail "could not open the test ssh session"
  x 'jodd-ai-host lock' >/dev/null
  x 'jodd-ai-host unlock' >/dev/null
  install -e JODD_AI_SKIP_CLIS=1 >/dev/null
  e2e_session_ping alive || { e2e_session_end; fail "an open ssh session died when sshd was reloaded"; }
  e2e_session_end
  echo "ok [$img]: an open ssh session survives lock, unlock and an installer re-run"

  # The helper runs the account's commands through `su -l`: root's environment must not reach them.
  # (the probe goes into the files bash reads at login: ~/.profile, and ~/.bash_profile where it exists)
  x 'for f in .profile .bash_profile; do [ -f /home/jodd-ai/$f ] && echo "env > /tmp/e2e-env.txt" >> /home/jodd-ai/$f; done; : > /tmp/e2e-env.txt; chmod 666 /tmp/e2e-env.txt'
  docker exec -e E2E_ROOT_SECRET=leaked "$name" jodd-ai-host status >/dev/null
  x 'grep -q "^HOME=/home/jodd-ai" /tmp/e2e-env.txt && grep -q "^USER=jodd-ai" /tmp/e2e-env.txt' || fail "the account's login environment was not captured"
  x 'grep -q "^E2E_ROOT_SECRET=" /tmp/e2e-env.txt' && fail "root's environment reached the AI account"
  x 'for f in .profile .bash_profile; do [ -f /home/jodd-ai/$f ] && sed -i "/e2e-env.txt/d" /home/jodd-ai/$f; done; : > /tmp/e2e-env.txt'
  echo "ok [$img]: root's environment does not reach the AI account"
  cleanup

  # ============================== swallow ===============================
  start swallow
  prep_sshd; empty_dropins
  # The Match block comes BEFORE the Include: sshd would swallow the Include into it.
  x 'printf "PasswordAuthentication no\nPermitRootLogin no\nMatch User someoneelse\n    X11Forwarding no\nInclude /etc/ssh/sshd_config.d/*.conf\n" > /etc/ssh/sshd_config'
  # control: with the drop-in in place but the Include where it is, the drop-in is swallowed
  # (printed, not asserted: it is the premise, and it depends on this OpenSSH version)
  x 'mkdir -p /etc/ssh/sshd_config.d; . /kit/lib/dropin.sh; render_dropin jodd-ai no > /etc/ssh/sshd_config.d/50-jodd-ai.conf
     echo "control [$(/usr/sbin/sshd -V 2>&1 | head -n 1)]: $(/usr/sbin/sshd -T -f /etc/ssh/sshd_config -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -i "^passwordauthentication")"'
  x ': > /etc/ssh/sshd_config.d/50-jodd-ai.conf'   # empty the control file; the installer writes the real one
  # An existing account that can become root is refused: Jodd runs AI CLIs as it, driven remotely.
  x 'if command -v useradd >/dev/null; then groupadd -f wheel; useradd -m -G wheel e2eadmin; else adduser -D e2eadmin; addgroup wheel 2>/dev/null; addgroup e2eadmin wheel; fi'
  s_a=$(snap)
  if out=$(install -e JODD_AI_USER=e2eadmin -e JODD_AI_SKIP_CLIS=1 2>&1); then fail "the installer accepted an account in the wheel group"; fi
  printf '%s\n' "$out" | grep -q "e2eadmin is in the 'wheel' group" || { printf '%s\n' "$out" | tail -n 4; fail "no message about the privileged account"; }
  [ "$s_a" = "$(snap)" ] || fail "the privileged-account refusal changed the sshd configuration"
  echo "ok [$img]: an account in the wheel group is refused, nothing touched"
  # no JODD_AI_PASSWORD and no terminal: one is generated and printed (at once, and in the summary)
  out=$(install -e JODD_AI_SKIP_CLIS=1 2>&1) || { echo "$out"; fail "the installer failed"; }
  printf '%s\n' "$out" | grep -E "adding 'Include|Include /etc/ssh/sshd_config.d/50-jodd-ai.conf'" | head -n 1
  gen=$(printf '%s\n' "$out" | sed -n 's/.*generated password for jodd-ai: \(.*\)$/\1/p')
  [ -n "$gen" ] || { echo "$out"; fail "no generated password was printed"; }
  # shown as soon as it is set, and again in the final summary
  printf '%s\n' "$out" | grep -qE '^  Password: +'"$gen" || fail "the summary does not repeat the generated password"
  x '/usr/sbin/sshd -t' || fail "sshd -t rejects the configuration"
  first=$(x "grep -Ev '^[[:space:]]*(#|\$)' /etc/ssh/sshd_config | head -n 1")
  [ "$first" = 'Include /etc/ssh/sshd_config.d/50-jodd-ai.conf' ] || fail "first non-comment line is '$first'"
  [ "$(x "grep -c 'Include' /etc/ssh/sshd_config")" = 2 ] || fail "expected exactly the admin's Include plus ours"
  x '/usr/sbin/sshd -T -C user=jodd-ai,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication yes"' || fail "jodd-ai cannot use a password"
  x '/usr/sbin/sshd -T -C user=root,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication no"' || fail "root's setting changed"
  x '/usr/sbin/sshd -T -C user=someoneelse,host=x,addr=127.0.0.1 | grep -qi "^passwordauthentication no"' || fail "someoneelse's setting changed"
  x '/usr/sbin/sshd -T -C user=someoneelse,host=x,addr=127.0.0.1 | grep -qi "^x11forwarding no"' || fail "the admin's Match block no longer applies"
  e2e_wait_port "$port" || fail "sshd is not reachable"
  make_user someoneelse "$pw"
  ssh_pw jodd-ai "$gen" || fail "password login failed for jodd-ai with the generated password"
  ssh_pw someoneelse "$pw" && fail "password login WORKS for another account"
  sum1=$(snap)
  out=$(install -e JODD_AI_SKIP_CLIS=1 2>&1)
  printf '%s\n' "$out" | grep -q "$gen" && fail "a re-run printed the generated password again"
  [ "$sum1" = "$(snap)" ] || fail "re-run changed the sshd configuration"
  echo "ok [$img]: the Include went first and a Match block above the old Include cannot swallow it"
  # Root never writes through a link the account controls. The account plants links where the
  # installer appends its PATH line (and, on Alpine, writes settings.json) pointing at root-owned
  # files; a re-run must leave their targets alone, still owned by root.
  x 'printf "orig\n" > /etc/e2e-target; chmod 600 /etc/e2e-target; printf "orig\n" > /etc/e2e-target2; chmod 600 /etc/e2e-target2
     cd /home/jodd-ai && rm -f .profile .bash_profile && ln -s /etc/e2e-target .profile && ln -s /etc/e2e-target2 .bash_profile
     rm -rf .claude && ln -s /etc/e2e-dir .claude; mkdir -p /etc/e2e-dir; chmod 700 /etc/e2e-dir'
  t_before=$(x 'sha256sum /etc/e2e-target /etc/e2e-target2; stat -c "%U %a %n" /etc/e2e-target /etc/e2e-target2 /etc/e2e-dir; ls /etc/e2e-dir | wc -l')
  out=$(install -e JODD_AI_SKIP_CLIS=1 2>&1) || { printf '%s\n' "$out" | tail -n 6; fail "re-run failed with links in the account's home (it should only warn)"; }
  [ "$t_before" = "$(x 'sha256sum /etc/e2e-target /etc/e2e-target2; stat -c "%U %a %n" /etc/e2e-target /etc/e2e-target2 /etc/e2e-dir; ls /etc/e2e-dir | wc -l')" ] \
    || fail "root wrote through a link in the account's home (target changed, or its owner/mode)"
  printf '%s\n' "$out" | grep -q 'could not add ~/.local/bin' || fail "no warning that the PATH line could not be added"
  echo "ok [$img]: root does not follow links in the account's home (targets unchanged, still root-owned)"
  # ... and root's environment does not reach the child that writes there (the account can read
  # /proc/PID/environ): a stand-in for su records the environment it was started with.
  x 'printf "#!/bin/sh\nenv > /tmp/e2e-su-env\nexec su \"\$@\"\n" > /tmp/e2e-fakesu; chmod 755 /tmp/e2e-fakesu; : > /tmp/e2e-su-env; chmod 666 /tmp/e2e-su-env'
  install -e JODD_AI_SKIP_CLIS=1 -e JODD_AI_SU=/tmp/e2e-fakesu -e E2E_ROOT_SECRET=leaked >/dev/null 2>&1 || fail "re-run with the su stand-in failed"
  x 'grep -q "^HOME=/home/jodd-ai" /tmp/e2e-su-env' || fail "the su stand-in was not used, or su got no HOME"
  x 'grep -q "^E2E_ROOT_SECRET=" /tmp/e2e-su-env' && fail "root's environment reached su for the writes into the account's home"
  echo "ok [$img]: root's environment does not reach the writes into the account's home"
  cleanup

  # ============================== rollback ==============================
  start rollback
  prep_sshd; empty_dropins
  # a base whose Include is NOT first, so the installer edits sshd_config before sshd rejects it
  x 'printf "PasswordAuthentication no\nMatch User someoneelse\n    X11Forwarding no\nInclude /etc/ssh/sshd_config.d/*.conf\n" > /etc/ssh/sshd_config; cp /etc/ssh/sshd_config /etc/sshd_config.good'
  # An interrupt between writing the sshd config and validating it restores it (test knob).
  s_i=$(snap)
  rc=0; out=$(install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 -e JODD_AI_TEST_INTERRUPT=after-config-write 2>&1) || rc=$?
  [ "$rc" = 130 ] || { printf '%s\n' "$out" | tail -n 8; fail "interrupt: exit code $rc, expected 130"; }
  printf '%s\n' "$out" | grep -q "adding 'Include" || fail "interrupt: sshd_config was not edited before the interrupt, so the rollback was not exercised"
  printf '%s\n' "$out" | grep -q 'Interrupted: the sshd configuration was restored' || fail "interrupt: no rollback message"
  [ "$s_i" = "$(snap)" ] || { echo "before:"; echo "$s_i"; echo "after:"; snap; fail "interrupt: the sshd configuration changed (or temp files were left)"; }
  echo "ok [$img]: an interrupt after the config was written restores sshd_config and the drop-in byte for byte"
  # The guard: sshd accepts the new configuration, but another account's effective settings would
  # change. A stand-in sshd earlier in PATH adds a line to root's `sshd -T` once the drop-in
  # exists (as an Include that leaked its Match block would); the installer must notice, roll
  # back, and exit 1.
  x 'mkdir -p /usr/local/sbin; printf "%s\n" "#!/bin/sh" "real=/usr/sbin/sshd" \
       "if [ -s /etc/guard.on ] && [ -s /etc/ssh/sshd_config.d/50-jodd-ai.conf ]; then" \
       "  case \" \$* \" in *\" -T \"*user=root*) \"\$real\" \"\$@\"; rc=\$?; echo x11forwarding yes; exit \$rc ;; esac" \
       "fi" "exec \"\$real\" \"\$@\"" > /usr/local/sbin/sshd; chmod 755 /usr/local/sbin/sshd; echo on > /etc/guard.on'
  s_g=$(snap)
  if out=$(install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 2>&1); then fail "guard: the installer succeeded although another account's settings would change"; fi
  printf '%s\n' "$out" | grep -q 'alter the effective sshd settings of other accounts (root)' || { echo "$out" | tail -n 8; fail "guard: no message about other accounts"; }
  printf '%s\n' "$out" | grep -q 'restored' || fail "guard: no 'restored' message"
  [ "$s_g" = "$(snap)" ] || { echo "before:"; echo "$s_g"; echo "after:"; snap; fail "guard: the sshd configuration changed after the failed run"; }
  x ': > /etc/guard.on'
  echo "ok [$img]: the guard refuses a change that would alter another account's settings, and restores"
  x 'printf "NotAnSshdKeyword yes\n" >> /etc/ssh/sshd_config'
  s0=$(snap)
  if out=$(install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 2>&1); then fail "the installer succeeded on a configuration sshd rejects"; fi
  printf '%s\n' "$out" | tail -n 6
  printf '%s\n' "$out" | grep -qi 'NotAnSshdKeyword' || fail "sshd's own message is not shown"
  printf '%s\n' "$out" | grep -q 'restored' || fail "no 'restored' message"
  printf '%s\n' "$out" | grep -q "adding 'Include" || fail "sshd_config was not edited before the failure, so its restore was not exercised"
  [ "$s0" = "$(snap)" ] || { echo "before:"; echo "$s0"; echo "after:"; snap; fail "the sshd configuration changed after the failed run"; }
  # same, with the drop-in already installed and the Include already added
  x 'cp /etc/sshd_config.good /etc/ssh/sshd_config'
  install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 >/dev/null || fail "the installer failed on a good configuration"
  x 'printf "NotAnSshdKeyword yes\n" >> /etc/ssh/sshd_config'
  s2=$(snap)
  if out=$(install -e JODD_AI_SKIP_CLIS=1 2>&1); then fail "second run: the installer succeeded on a configuration sshd rejects"; fi
  [ "$s2" = "$(snap)" ] || { echo "before:"; echo "$s2"; echo "after:"; snap; fail "the sshd configuration changed after the failed second run"; }
  echo "ok [$img]: a configuration sshd rejects -> exit 1 with sshd's message, sshd_config and sshd_config.d byte for byte unchanged (fresh and with a drop-in already there)"
  cleanup

  # ============================== authgate (ubuntu:24.04 only) ==========
  # The drop-in sets PasswordAuthentication only. A global `AuthenticationMethods publickey` and an
  # AllowUsers that leaves the account out still refuse it; they are the admin's, so the
  # installer must warn naming them and change neither, and doctor must print FIX lines.
  if [ "$img" = ubuntu:24.04 ]; then
    start authgate
    x 'printf "AuthenticationMethods publickey\nAllowUsers root someoneelse\n" > /etc/ssh.seed'
    out=$(install -e JODD_AI_PASSWORD="$pw" -e JODD_AI_SKIP_CLIS=1 -e JODD_AI_SEED_CONFIG=/etc/ssh.seed 2>&1) \
      || { printf '%s\n' "$out" | tail -n 8; fail "the installer failed; it should only warn"; }
    printf '%s\n' "$out" | grep -q 'WARNING: another sshd setting keeps jodd-ai out' || { printf '%s\n' "$out" | tail -n 20; fail "no warning about the settings that keep jodd-ai out"; }
    printf '%s\n' "$out" | grep -q "AuthenticationMethods is 'publickey' for jodd-ai" || fail "the warning does not name AuthenticationMethods"
    printf '%s\n' "$out" | grep -q 'AllowUsers does not name jodd-ai' || fail "the warning does not name AllowUsers"
    printf '%s\n' "$out" | grep -E '^    - (AuthenticationMethods|AllowUsers)'
    x 'grep -qx "AuthenticationMethods publickey" /etc/ssh/sshd_config && grep -qx "AllowUsers root someoneelse" /etc/ssh/sshd_config' \
      || fail "the installer changed the admin's AuthenticationMethods / AllowUsers"
    x 'grep -Eqi "AuthenticationMethods|AllowUsers" /etc/ssh/sshd_config.d/50-jodd-ai.conf' && fail "the drop-in overrides the admin's AuthenticationMethods / AllowUsers"
    # control: the warning is real - the password the installer set is refused
    e2e_wait_port "$port" || fail "sshd is not reachable on $port"
    ssh_pw jodd-ai "$pw" && fail "control: password login worked although AuthenticationMethods is publickey"
    rc=0; d=$(x 'jodd-ai-host doctor') || rc=$?
    [ "$rc" = 1 ] || { echo "$d"; fail "doctor passed (rc $rc) on a server that refuses jodd-ai"; }
    printf '%s\n' "$d" | grep -E '^FIX .*(AuthenticationMethods|AllowUsers)'
    printf '%s\n' "$d" | grep -q "^FIX .*AuthenticationMethods is 'publickey'" || { echo "$d"; fail "doctor prints no FIX for AuthenticationMethods"; }
    printf '%s\n' "$d" | grep -q '^FIX .*AllowUsers does not name jodd-ai' || { echo "$d"; fail "doctor prints no FIX for AllowUsers"; }
    echo "ok [$img]: AuthenticationMethods publickey / AllowUsers without jodd-ai -> the installer warns and changes neither, doctor prints FIX"
    cleanup
  fi
  echo "linux e2e [$img]: OK"
done

# Refusals before anything is touched (once, not per image): no known package manager; not root.
name=jodd-ai-e2e-refusals
docker rm -f "$name" >/dev/null 2>&1 || :
if out=$(docker run --rm --name "$name" -v "$repo/ai-host:/kit:ro" busybox sh /kit/install/linux.sh 2>&1); then echo "FAIL: ran on a system with no apt, dnf or apk"; exit 1; fi
printf '%s\n' "$out" | grep -q 'Unsupported package manager' || { echo "FAIL: no 'Unsupported package manager' message: $out"; exit 1; }
printf '%s\n' "$out" | grep -q 'INSTALL.md#other-linux' || { echo "FAIL: no link to the manual steps: $out"; exit 1; }
if out=$(docker run --rm --name "$name" --user 1000 -v "$repo/ai-host:/kit:ro" busybox sh /kit/install/linux.sh 2>&1); then echo "FAIL: ran as a non-root user"; exit 1; fi
printf '%s\n' "$out" | grep -q 'run this as root' || { echo "FAIL: no root message: $out"; exit 1; }
name=
echo "ok: refuses an unsupported package manager (with the manual link) and a non-root user"
