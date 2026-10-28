#!/bin/sh
# shellcheck disable=SC2012  # ls is only used on files this test created, with plain names
# shellcheck disable=SC2016  # fake scripts and expected strings hold a literal $ for the shell that runs them
# Tests for the parts of jodd-ai-host that touch only files. Run: sh ai-host/test/helper_test.sh
set -u
here=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
pass=0 fail=0
ok()   { pass=$((pass+1)); }
bad()  { fail=$((fail+1)); echo "FAIL: $1"; }
check(){ if [ "$2" = "$3" ]; then ok; else bad "$1: expected [$3] got [$2]"; fi; }
# has DESC TEXT PATTERN: TEXT contains a line matching the grep pattern.
has()  { if printf '%s\n' "$2" | grep -q -- "$3"; then ok; else bad "$1"; fi; }
# hasnt DESC TEXT PATTERN: no line matches.
hasnt(){ if printf '%s\n' "$2" | grep -q -- "$3"; then bad "$1"; else ok; fi; }
# infile DESC FILE PATTERN (binary-safe)
infile(){ if LC_ALL=C grep -aqF -- "$3" "$2"; then ok; else bad "$1"; fi; }

# shellcheck disable=SC1091
. "$here/lib/dropin.sh"

# render_dropin: Match block for the account only, password on/off.
out=$(render_dropin jodd-ai no)
has "dropin has Match User" "$out" '^Match User jodd-ai$'
has "unlocked = yes" "$out" '^    PasswordAuthentication yes$'
has "keepalive in Match" "$out" '^    ClientAliveInterval 30$'
out=$(render_dropin jodd-ai yes)
has "locked = no" "$out" '^    PasswordAuthentication no$'
has "locked kbd = no" "$out" '^    KbdInteractiveAuthentication no$'
hasnt "locked never says yes" "$out" 'PasswordAuthentication yes'

# sshd_login_problems: the sshd settings besides PasswordAuthentication that keep the account out.
# Input is `sshd -T` output; the default (no Allow/Deny lines, AuthenticationMethods any) is fine.
slp() { printf '%b' "$1" | sshd_login_problems jodd-ai "jodd-ai users"; }
check "default sshd -T: no problem" "$(slp 'passwordauthentication yes\nauthenticationmethods any\n')" ""
check "no authenticationmethods line: no problem" "$(slp 'passwordauthentication yes\n')" ""
has "AuthenticationMethods publickey is a problem, named" "$(slp 'authenticationmethods publickey\n')" "^AuthenticationMethods is 'publickey' for jodd-ai"
has "publickey,password is a problem (password must work alone)" "$(slp 'authenticationmethods publickey,password\n')" '^AuthenticationMethods'
check "a separate 'password' list is fine" "$(slp 'authenticationmethods publickey password\n')" ""
has "AllowUsers without the account is a problem, named" "$(slp 'allowusers root\nallowusers admin\n')" '^AllowUsers does not name jodd-ai'
check "AllowUsers naming the account is fine" "$(slp 'allowusers root\nallowusers jodd-ai\n')" ""
check "an AllowUsers wildcard that matches is fine" "$(slp 'allowusers jodd-*\n')" ""
has "AllowUsers user@host: cannot tell, says so" "$(slp 'allowusers root\nallowusers jodd-ai@10.0.0.*\n')" '^AllowUsers lets jodd-ai in only from some hosts'
has "DenyUsers naming the account is a problem" "$(slp 'denyusers jodd-ai\n')" '^DenyUsers names jodd-ai'
has "DenyUsers *@host: cannot tell, says so" "$(slp 'denyusers *@192.0.2.1\n')" "^DenyUsers has '\*@192.0.2.1'"
check "DenyUsers of someone else is fine" "$(slp 'denyusers mallory\n')" ""
has "AllowGroups without the account's groups is a problem" "$(slp 'allowgroups sshusers\n')" "^AllowGroups names none of jodd-ai's groups (jodd-ai users)"
check "AllowGroups with one of its groups is fine" "$(slp 'allowgroups sshusers\nallowgroups users\n')" ""
has "DenyGroups with one of its groups is a problem" "$(slp 'denygroups users\n')" '^DenyGroups names users'
check "a [ pattern is compared literally" "$(slp 'allowusers jodd-a[i]\n' | grep -c '^AllowUsers')" "1"

# A fake host the helper can manage.
mkdir -p "$t/home/.ssh" "$t/etc"
cat > "$t/conf" <<CONF
JODD_AI_USER=$(id -un)
JODD_AI_PORT=1
JODD_AI_KIND=linux
JODD_AI_DROPIN=$t/etc/50-jodd-ai.conf
JODD_AI_HOSTKEY=$t/hostkey.pub
JODD_AI_LIB=$here/lib
JODD_AI_HOME=$t/home
CONF
ssh-keygen -q -t ed25519 -N '' -f "$t/hostkey" >/dev/null
render_dropin "$(id -un)" no > "$t/etc/50-jodd-ai.conf"
export JODD_AI_HOST_CONF="$t/conf" JODD_AI_SSHD_RELOAD=true JODD_AI_SSHD_TEST=true JODD_AI_ALLOW_NONROOT=1
H="$here/bin/jodd-ai-host"
# The same-user branch of as_user runs the account's login shell; pin it so results don't depend on the developer's.
export JODD_AI_SHELL=/bin/sh
# mkconf NAME HOME HOSTKEY.pub: a variant of the fake host's conf.
mkconf() { sed -e "s|^JODD_AI_HOME=.*|JODD_AI_HOME=$2|" -e "s|^JODD_AI_HOSTKEY=.*|JODD_AI_HOSTKEY=$3|" "$t/conf" > "$t/$1"; }
# modeof FILE: the ls -l permission string; ownerof FILE: numeric uid.
modeof()  { ls -l "$1" | cut -c1-10; }
ownerof() { ls -ln "$1" | awk '{print $3}'; }

# fingerprint matches ssh-keygen's, in Jodd's "ED25519 SHA256:..." form
want="ED25519 $(ssh-keygen -lf "$t/hostkey.pub" -E sha256 | awk '{print $2}')"
check "fingerprint" "$(sh "$H" fingerprint)" "$want"

# fingerprint fails (rc 1, no output) when the host key cannot be read
mkconf conf-nokey "$t/home" "$t/missing.pub"
out=$(JODD_AI_HOST_CONF="$t/conf-nokey" sh "$H" fingerprint 2>/dev/null); rc=$?
check "fingerprint fails on an unreadable key (rc)" "$rc" "1"
check "fingerprint prints nothing on failure" "$out" ""

# lock / unlock flip only the PasswordAuthentication line
sh "$H" lock >/dev/null
infile "lock wrote no" "$t/etc/50-jodd-ai.conf" 'PasswordAuthentication no'
sh "$H" unlock >/dev/null
infile "unlock wrote yes" "$t/etc/50-jodd-ai.conf" 'PasswordAuthentication yes'

# lock restores the old file when sshd -t fails
cp "$t/etc/50-jodd-ai.conf" "$t/before"
JODD_AI_SSHD_TEST=false sh "$H" lock >/dev/null 2>&1; rc=$?
check "lock refuses on bad config (rc)" "$rc" "1"
if cmp -s "$t/before" "$t/etc/50-jodd-ai.conf"; then ok; else bad "lock restored the previous drop-in"; fi

# a stale .prev from an earlier crash must not be restored in place of the real previous drop-in
printf 'STALE\n' > "$t/etc/50-jodd-ai.conf.prev"
cp "$t/etc/50-jodd-ai.conf" "$t/before"
JODD_AI_SSHD_TEST=false sh "$H" lock >/dev/null 2>&1; rc=$?
check "rejected lock with a stale .prev (rc)" "$rc" "1"
if cmp -s "$t/before" "$t/etc/50-jodd-ai.conf"; then ok; else bad "stale .prev was restored over the live drop-in"; fi
if [ -e "$t/etc/50-jodd-ai.conf.prev" ]; then bad ".prev left behind"; else ok; fi
# no previous drop-in + sshd rejects: no file is left behind, no temp files either
rm -f "$t/etc/50-jodd-ai.conf"
printf 'STALE\n' > "$t/etc/50-jodd-ai.conf.prev"   # ... even with a stale .prev lying around
JODD_AI_SSHD_TEST=false sh "$H" unlock >/dev/null 2>&1; rc=$?
check "rejected first write (rc)" "$rc" "1"
check "rejected first write leaves the drop-in dir empty" "$(ls -A "$t/etc" | wc -l | tr -d ' ')" "0"
render_dropin "$(id -un)" no > "$t/etc/50-jodd-ai.conf"
sh "$H" lock >/dev/null
check "a good write leaves no temp or .prev files" "$(ls -A "$t/etc")" "50-jodd-ai.conf"
sh "$H" unlock >/dev/null

# keys / revoke: only jodd-ai-<id> lines, only the named one removed, foreign non-UTF-8 kept byte-exact
{
  printf 'ssh-ed25519 AAAAone alice@laptop\n'
  printf 'restrict,command="x --key 11111111" ssh-ed25519 AAAAx jodd-ai-11111111\n'
  printf 'ssh-rsa AAAAtwo caf\351@latin1\n'
  printf 'restrict,command="x --key 22222222" ssh-ed25519 AAAAy jodd-ai-22222222\n'
} > "$t/home/.ssh/authorized_keys"
check "keys lists two" "$(sh "$H" keys | grep -c jodd-ai-)" "2"
LC_ALL=C.UTF-8 sh "$H" revoke 11111111 >/dev/null
check "revoke left one jodd key" "$(grep -ac jodd-ai- "$t/home/.ssh/authorized_keys")" "1"
infile "other jodd key kept" "$t/home/.ssh/authorized_keys" 'jodd-ai-22222222'
infile "foreign non-UTF-8 line kept" "$t/home/.ssh/authorized_keys" "caf$(printf '\351')@latin1"
sh "$H" revoke nothex >/dev/null 2>&1; check "revoke refuses a non-hex id" "$?" "2"

# revoke of an id that is not there: says so, exits 1, and does not touch the file
cp -p "$t/home/.ssh/authorized_keys" "$t/ak.before"
out=$(sh "$H" revoke bbbbbbbb 2>&1); rc=$?
check "revoke of an absent id (rc)" "$rc" "1"
has "revoke of an absent id says so" "$out" 'No key jodd-ai-bbbbbbbb'
hasnt "revoke of an absent id does not claim removal" "$out" 'Removed'
if cmp -s "$t/ak.before" "$t/home/.ssh/authorized_keys"; then ok; else bad "absent-id revoke changed the file"; fi

# revoke is atomic and keeps mode (and owner); non-UTF-8 and a last line with no newline survive
{
  printf 'ssh-rsa AAAAfoo caf\351@latin1\n'
  printf 'restrict,command="x --key 33333333" ssh-ed25519 AAAAz jodd-ai-33333333\n'
  printf 'ssh-ed25519 AAAAlast nonewline'
} > "$t/home/.ssh/authorized_keys"
chmod 640 "$t/home/.ssh/authorized_keys"
if [ "$(id -u)" = 0 ]; then chown 12345 "$t/home/.ssh/authorized_keys"; fi
want_owner=$(ownerof "$t/home/.ssh/authorized_keys")
sh "$H" revoke 33333333 >/dev/null; check "revoke of the middle key (rc)" "$?" "0"
check "revoke kept the mode" "$(modeof "$t/home/.ssh/authorized_keys")" "-rw-r-----"
check "revoke kept the owner" "$(ownerof "$t/home/.ssh/authorized_keys")" "$want_owner"
infile "latin1 line survived" "$t/home/.ssh/authorized_keys" "caf$(printf '\351')@latin1"
infile "last line without newline survived" "$t/home/.ssh/authorized_keys" "ssh-ed25519 AAAAlast nonewline"
check "revoke removed the key" "$(grep -ac jodd-ai- "$t/home/.ssh/authorized_keys")" "0"
check "revoke leaves no temp files in .ssh" "$(ls -A "$t/home/.ssh")" "authorized_keys"

# revoke refuses a symlinked authorized_keys without touching anything
mkdir -p "$t/home2/.ssh"
printf 'restrict,command="x --key 44444444" ssh-ed25519 AAAAq jodd-ai-44444444\n' > "$t/real_keys"
ln -s "$t/real_keys" "$t/home2/.ssh/authorized_keys"
cp -p "$t/real_keys" "$t/real_keys.before"
mkconf conf-link "$t/home2" "$t/hostkey.pub"
out=$(JODD_AI_HOST_CONF="$t/conf-link" sh "$H" revoke 44444444 2>&1); rc=$?
check "revoke on a symlinked file (rc)" "$rc" "1"
has "revoke names the symlink" "$out" 'symlink'
if cmp -s "$t/real_keys.before" "$t/real_keys" && [ -L "$t/home2/.ssh/authorized_keys" ]; then ok; else bad "symlink case changed something"; fi
check "symlink case leaves no temp files" "$(ls -A "$t/home2/.ssh")" "authorized_keys"

# doctor: must fail on this fake host and name the CLIs (nothing listens on port 1, so it is
# deterministic even on a dev machine that has claude/codex on PATH). It needs no root for read-only checks.
sh "$H" doctor >"$t/d" 2>&1; check "doctor fails without CLIs" "$?" "1"
infile "doctor names claude" "$t/d" 'claude'

# doctor reads sshd's effective settings (JODD_AI_SSHD_DUMP stands in for `sshd -T`, which needs
# root): a global AuthenticationMethods publickey and an AllowUsers without the account are FIX
# lines naming the setting, even though PasswordAuthentication itself is yes.
printf 'passwordauthentication yes\nauthenticationmethods publickey\nallowusers jodd-test-nobody\n' > "$t/sshdT"
out=$(JODD_AI_SSHD_DUMP="cat $t/sshdT" sh "$H" doctor 2>&1)
has "doctor: password login itself is ok" "$out" '^ok .*sshd allows password login'
has "doctor: FIX names AuthenticationMethods" "$out" "^FIX .*AuthenticationMethods is 'publickey'"
has "doctor: FIX names AllowUsers" "$out" '^FIX .*AllowUsers does not name'
printf 'passwordauthentication yes\nauthenticationmethods any\nallowgroups jodd-test-nosuchgroup\n' > "$t/sshdT"
out=$(JODD_AI_SSHD_DUMP="cat $t/sshdT" sh "$H" doctor 2>&1)
has "doctor: FIX names AllowGroups" "$out" '^FIX .*AllowGroups names none of'
hasnt "doctor: AuthenticationMethods any is not a FIX" "$out" '^FIX .*AuthenticationMethods'
printf 'passwordauthentication yes\nauthenticationmethods any\nallowusers %s\n' "$(id -un)" > "$t/sshdT"
out=$(JODD_AI_SSHD_DUMP="cat $t/sshdT" sh "$H" doctor 2>&1)
has "doctor: ok when nothing keeps the account out" "$out" '^ok .*AllowUsers/AllowGroups/DenyUsers/DenyGroups let'
hasnt "doctor: no access FIX then" "$out" '^FIX .*\(AuthenticationMethods\|AllowUsers\|AllowGroups\|Deny\)'

# doctor in the Docker image: JODD_AI_PORT is the published port; sshd listens on 22 inside.
mkdir -p "$t/ncbin"
printf '#!/bin/sh\necho "$@" >> "%s/nc.log"\n[ "$3" = 22 ]\n' "$t" > "$t/ncbin/nc"; chmod +x "$t/ncbin/nc"
sed -e 's/^JODD_AI_KIND=.*/JODD_AI_KIND=docker/' -e 's/^JODD_AI_PORT=.*/JODD_AI_PORT=2222/' "$t/conf" > "$t/conf.docker"
out=$(JODD_AI_HOST_CONF="$t/conf.docker" PATH="$t/ncbin:$PATH" sh "$H" doctor 2>&1)
has "docker doctor checks port 22 inside the container" "$out" '^ok .*listening on port 22 inside the container (published as 2222)'
hasnt "docker doctor does not probe the published port" "$(cat "$t/nc.log")" ' 2222$'
: > "$t/nc.log"
out=$(PATH="$t/ncbin:$PATH" sh "$H" doctor 2>&1)
has "linux doctor checks JODD_AI_PORT itself" "$out" '^FIX .*nothing is listening on port 1\.'

# doctor finds the CLIs the way Jodd's Setup does: login shell -lc first, then -lic; says which
mkdir -p "$t/bin2"
printf '#!/bin/sh\necho fake\n' > "$t/bin2/claude"; chmod +x "$t/bin2/claude"
cp "$t/bin2/claude" "$t/bin2/codex"
SYSPATH=/usr/bin:/bin:/usr/sbin:/sbin
doc() { env PATH="$SYSPATH" sh "$H" doctor 2>&1; }
echo "export PATH=\"$t/bin2:\$PATH\"" > "$t/home/.profile"
out=$(doc)
has "doctor finds claude via -lc" "$out" '^ok .*claude found.*-lc[^i]'
echo : > "$t/home/.profile"
echo "export PATH=\"$t/bin2:\$PATH\"" > "$t/home/.shrc"
out=$(ENV="$t/home/.shrc" doc)
has "doctor finds claude via -lic when -lc cannot" "$out" '^ok .*claude found.*-lic'
echo : > "$t/home/.shrc"
mkdir -p "$t/home/.local/bin"; cp "$t/bin2/claude" "$t/home/.local/bin/claude"
out=$(doc)
has "doctor explains a CLI outside the login PATH" "$out" "^FIX .*claude is installed in .*local/bin.*doesn't search"
has "doctor says codex is not installed" "$out" '^FIX .*codex is not installed'

# status and doctor probe the CLIs through the account's login shell, whose profile the account
# controls: those probes must not get the caller's stdin (root's terminal, under sudo).
rm -f "$t/home/.local/bin/claude"
echo 'if read -r l; then echo "got:$l" >> "'"$t"'/stdin.log"; fi' > "$t/home/.profile"
: > "$t/stdin.log"
echo ROOT-TERMINAL-INPUT | doc >/dev/null
echo ROOT-TERMINAL-INPUT | env PATH="$SYSPATH" sh "$H" status >/dev/null 2>&1
check "status/doctor probes run with stdin from /dev/null" "$(cat "$t/stdin.log")" ""
echo : > "$t/home/.profile"

# update-clis: no network -> says how to retry and exits 1 (the image entrypoint relies on that).
# A fake curl that always fails stands in for "no network"; the login shell finds it via the fake home's .profile.
mkdir -p "$t/bin"
printf '#!/bin/sh\nexit 6\n' > "$t/bin/curl"; chmod +x "$t/bin/curl"
echo "export PATH=\"$t/bin:\$PATH\"" > "$t/home/.profile"
printf '#!/bin/sh\necho "echo installer-ran"\n' > "$t/bin/curl"
# (Claude's installer is run with bash, which a minimal image such as alpine may lack: then only Codex's runs.)
sh "$H" update-clis >"$t/u" 2>&1; rc=$?
if command -v bash >/dev/null 2>&1; then want=2 wantrc=0; else want=1 wantrc=1; fi
check "update-clis rc when the installers run" "$rc" "$wantrc"
check "update-clis ran the installers" "$(grep -c installer-ran "$t/u")" "$want"
# Codex's installer asks "Start Codex now?" on /dev/tty unless CODEX_NON_INTERACTIVE is set (a
# live macOS run answered y, ran Codex mid-install and reported the install as failed).
printf '#!/bin/sh\necho "echo nonint=\\${CODEX_NON_INTERACTIVE:-unset}"\n' > "$t/bin/curl"
sh "$H" update-clis >"$t/u" 2>&1
check "update-clis runs Codex's installer non-interactively" "$(grep -c '^nonint=1$' "$t/u")" "1"
# Run from a directory that no longer exists (a live macOS run printed a getcwd error for every
# shell it started as the account). bash-as-sh still prints two of its own before the helper's
# `cd /` (shell-init, and that cd); every shell started after it must print none.
mkdir "$t/gone"
(cd "$t/gone" && rmdir "$t/gone" && sh "$H" update-clis) >"$t/u" 2>&1
check "update-clis from a deleted directory: no getcwd errors after the helper's cd" "$(grep -c getcwd "$t/u" | awk '{print ($1 <= 2)}')" "1"
printf '#!/bin/sh\nexit 6\n' > "$t/bin/curl"
sh "$H" update-clis >"$t/u" 2>&1; check "update-clis fails without network (rc)" "$?" "1"
infile "update-clis names the retry command" "$t/u" 'jodd-ai-host update-clis'
infile "update-clis reports Claude" "$t/u" 'Claude Code did not install'
infile "update-clis reports Codex" "$t/u" 'Codex did not install'

# macOS: the account's commands go through `sudo -H -u USER env PATH=... SHELL -lc STRING`, one
# string, nothing for sudo to re-escape (sudo -i would leave `$` live in the outer login shell).
# A fake sudo logs its arguments, and with FAKE_SUDO_EXEC=1 runs what it was given.
mkdir -p "$t/fakebin"
cat > "$t/fakebin/sudo" <<'FAKE'
#!/bin/sh
: > "$FAKE_SUDO_LOG"
for a in "$@"; do printf '%s\n' "ARG<$a>" >> "$FAKE_SUDO_LOG"; done
[ -n "${FAKE_SUDO_EXEC:-}" ] || exit 0
[ "$1" = -H ] && [ "$2" = -u ] || { echo "fake sudo: unexpected arguments" >&2; exit 64; }
shift 3
HOME=$FAKE_SUDO_HOME exec "$@"
FAKE
chmod +x "$t/fakebin/sudo"
sed -e 's|^JODD_AI_KIND=.*|JODD_AI_KIND=macos|' -e 's|^JODD_AI_USER=.*|JODD_AI_USER=somebody-else|' "$t/conf" > "$t/conf-macos"
export FAKE_SUDO_LOG="$t/sudo.log" FAKE_SUDO_HOME="$t/home"
MPATH="$t/fakebin:$SYSPATH"
env PATH="$MPATH" JODD_AI_HOST_CONF="$t/conf-macos" sh "$H" update-clis >/dev/null 2>&1
infile "macOS: sudo -H -u USER, no -i" "$t/sudo.log" 'ARG<-H>'
hasnt "macOS: never sudo -i" "$(cat "$t/sudo.log")" 'ARG<-i>'
infile "macOS: the account name reaches sudo" "$t/sudo.log" 'ARG<somebody-else>'
infile "macOS: the account's shell runs -lc" "$t/sudo.log" 'ARG<-lc>'
infile "macOS: \$(...) is not escaped or expanded on the way" "$t/sudo.log" '$(curl -fsSL https://chatgpt.com/codex/install.sh)'
infile "macOS: cd to the account's home first" "$t/sudo.log" 'cd "$HOME" 2>/dev/null; sh -c '
infile "macOS: PATH is what sshd gives" "$t/sudo.log" 'ARG<PATH=/usr/bin:/bin:/usr/sbin:/sbin>'
printf '#!/bin/sh\npwd -P\necho "shell=$SHELL"\necho "home=$HOME"\necho "path=$PATH"\n' > "$t/bin2/codex"
echo "export PATH=\"$t/bin2:\$PATH\"" > "$t/home/.profile"
out=$(cd / && FAKE_SUDO_EXEC=1 env PATH="$MPATH" JODD_AI_HOST_CONF="$t/conf-macos" sh "$H" signin codex 2>&1)
has "macOS: runs in the account's home" "$out" "^$(cd "$t/home" && pwd -P)\$"
has "macOS: SHELL is the account's shell" "$out" '^shell=/bin/sh$'
has "macOS: HOME is the account's" "$out" "^home=$t/home\$"
hasnt "macOS: the caller's PATH does not leak" "$out" "fakebin"
# macOS needs no sshd reload (launchd starts a fresh sshd per connection): `lock` succeeds without
# touching launchctl, whose `kickstart -k` would kill live sessions.
printf '#!/bin/sh\necho called > "$FAKE_SUDO_LOG.launchctl"\nexit 1\n' > "$t/fakebin/launchctl"; chmod +x "$t/fakebin/launchctl"
env PATH="$MPATH" JODD_AI_HOST_CONF="$t/conf-macos" JODD_AI_SSHD_RELOAD= sh "$H" lock >/dev/null 2>&1; check "macOS lock succeeds without a reload" "$?" "0"
check "macOS lock never calls launchctl" "$([ -e "$t/sudo.log.launchctl" ] && echo called || echo no)" "no"
# revoke runs as the account (root never edits inside ~/.ssh): through sudo -H -u on macOS.
mkdir -p "$t/home3/.ssh"
printf 'ssh-ed25519 AAAA keep\nssh-ed25519 BBBB jodd-ai-55555555\n' > "$t/home3/.ssh/authorized_keys"
sed "s|^JODD_AI_HOME=.*|JODD_AI_HOME=$t/home3|" "$t/conf-macos" > "$t/conf-macos3"
out=$(FAKE_SUDO_EXEC=1 env PATH="$MPATH" JODD_AI_HOST_CONF="$t/conf-macos3" sh "$H" revoke 55555555 2>&1); rc=$?
check "macOS revoke (rc)" "$rc" "0"
has "macOS revoke says removed" "$out" 'Removed key jodd-ai-55555555'
check "macOS revoke removed only that key" "$(cat "$t/home3/.ssh/authorized_keys")" "ssh-ed25519 AAAA keep"
infile "macOS revoke goes through sudo -H -u as /bin/sh -c" "$t/sudo.log" 'ARG</bin/sh>'
rm -f "$t/fakebin/sudo" "$t/fakebin/launchctl"

# Linux: revoke runs as the account through su, and root's environment does not come along.
printf '#!/bin/sh\nenv > %s\nexec "$2" -c "$4"\n' "$t/su.env" > "$t/fakesu"
chmod +x "$t/fakesu"
mkdir -p "$t/home4/.ssh"
printf 'ssh-ed25519 AAAA keep\nssh-ed25519 BBBB jodd-ai-66666666\n' > "$t/home4/.ssh/authorized_keys"
sed -e 's|^JODD_AI_USER=.*|JODD_AI_USER=somebody-else|' -e "s|^JODD_AI_HOME=.*|JODD_AI_HOME=$t/home4|" "$t/conf" > "$t/conf-linux4"
out=$(E2E_ROOT_SECRET=leaked JODD_AI_SU="$t/fakesu" JODD_AI_HOST_CONF="$t/conf-linux4" sh "$H" revoke 66666666 2>&1); rc=$?
check "linux revoke through su (rc)" "$rc" "0"
check "linux revoke removed only that key" "$(cat "$t/home4/.ssh/authorized_keys")" "ssh-ed25519 AAAA keep"
hasnt "root's environment does not reach su" "$(cat "$t/su.env")" '^E2E_ROOT_SECRET='
has "su gets the account's HOME" "$(cat "$t/su.env")" "^HOME=$t/home4\$"
has "su gets a plain PATH" "$(cat "$t/su.env")" '^PATH=/usr/bin:/bin$'

# usage
sh "$H" nosuchcommand >/dev/null 2>&1; check "unknown command exits 2" "$?" "2"

echo "helper tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
