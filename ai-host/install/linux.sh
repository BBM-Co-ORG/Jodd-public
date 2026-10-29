#!/bin/sh
# Jodd AI host kit - Linux installer (apt, dnf, apk).
#
# Prepares this server so Jodd can run YOUR OWN claude / codex CLI on it, signed in
# with YOUR OWN account, over a restricted SSH key. Read it before you run it:
#
#   curl -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/linux.sh -o jodd-ai-host-install.sh
#   sudo sh jodd-ai-host-install.sh
#
# What it does, in order (re-running repairs instead of duplicating):
#   1. installs OpenSSH server + curl, bash and friends with your package manager
#   2. creates the AI account (bash login shell, ~/.local/bin on its login PATH)
#   3. sets that account's password (see JODD_AI_PASSWORD) - the password is only for
#      Jodd's one-time Setup; afterwards `sudo jodd-ai-host lock` turns it off
#   4. creates missing SSH host keys (never replaces existing ones)
#   5. writes /etc/ssh/sshd_config.d/50-jodd-ai.conf, a `Match User <account>` block that
#      allows password login for THAT account only, and makes sure sshd_config reads it
#      ahead of any Match block of yours. It is validated with `sshd -t`, and the
#      effective settings of other accounts are compared before and after; if anything
#      is off, sshd_config and the drop-in are restored byte for byte and nothing else
#      is changed
#   6. installs claude and codex as the account, from their official installers
#   7. enables and (re)loads sshd
#   8. installs /usr/local/bin/jodd-ai-host and prints what to do next
#
# Environment:
#   JODD_AI_USER      account name (default jodd-ai)
#   JODD_AI_PASSWORD  its password. Without it: you are asked (twice) when run from a
#                     terminal, otherwise one is generated and printed (as soon as it is set,
#                     and again in the final summary). An existing
#                     account's password is left alone unless this value CHANGED.
#   JODD_AI_PORT      only what the final summary reports (e.g. a forwarded port);
#                     sshd's own port is not changed
#   JODD_AI_KIT_URL   where bin/ and lib/ are fetched from when this script is not run
#                     from a checkout of the kit
#                     (default https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host)
#   JODD_AI_SKIP_CLIS=1   do not install claude / codex (offline runs; `jodd-ai-host update-clis` later)
#
# Test-only knobs (used by scripts/ai-host-e2e/linux.sh; never needed on a real server):
#   JODD_AI_START_SSHD=1      when no init system runs (containers), start /usr/sbin/sshd directly
#   JODD_AI_SEED_CONFIG=FILE  append FILE to /etc/ssh/sshd_config after the packages are
#                             installed and before anything else, to simulate an admin's config
#   JODD_AI_SU=PATH           use this program instead of `su` for the writes into the account's home,
#                             to prove root's environment does not reach them
#   JODD_AI_TEST_INTERRUPT=after-config-write
#                             send this script an INT right after it wrote the sshd config and
#                             before it validated it, to prove the interrupt rolls the config back
#
# Needs root. POSIX sh.
set -eu

# Where bin/ and lib/ are, if we run from a checkout - found before the cd below, as $0 may be relative.
case $0 in */*) here=$(cd "$(dirname "$0")" && pwd) ;; *) here= ;; esac
local_kit=
if [ -n "$here" ] && [ -f "$0" ] && [ -f "$here/../bin/jodd-ai-host" ] && [ -f "$here/../lib/dropin.sh" ] && [ -f "$here/../lib/doctor.sh" ]; then
  local_kit=$(cd "$here/.." && pwd)
fi
# Then leave the caller's directory: it may be gone, or closed to the AI account, and every shell
# started as that account would print "getcwd: cannot access parent directories".
cd /

user=${JODD_AI_USER:-jodd-ai}
kit_url=${JODD_AI_KIT_URL:-https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host}
sshd_config=/etc/ssh/sshd_config
dropin_dir=/etc/ssh/sshd_config.d
dropin=$dropin_dir/50-jodd-ai.conf
libdir=/usr/local/lib/jodd-ai-host
helper=/usr/local/bin/jodd-ai-host
state=/var/lib/jodd-ai-host
manual_url=https://github.com/BBM-Co-ORG/Jodd-public/blob/main/docs/ai-host/INSTALL.md

say()  { printf '%s\n' "jodd-ai-host: $*"; }
warn() { printf '%s\n' "jodd-ai-host: $*" >&2; }
die()  { warn "$*"; exit 1; }

work=
generated=
config_pending=0   # 1 from the first write to sshd's config until it has been validated
cleanup() {
  _rc=$?
  # Ctrl-C at the password prompt must not leave the terminal without echo.
  if [ -t 0 ]; then stty echo 2>/dev/null || :; fi
  # An interrupt between writing the sshd config and validating it: put back what was there.
  if [ "$config_pending" = 1 ]; then
    rollback
    warn "Interrupted: the sshd configuration was restored to exactly what it was."
  fi
  rm -f "$dropin_dir"/.jodd-dropin.* "$(dirname "$sshd_config")"/.jodd-sshd_config.* 2>/dev/null || :
  if [ -n "$work" ]; then rm -rf "$work"; fi
  return $_rc
}
# dash does not run the EXIT trap when a signal kills the shell; turn the signals into exits.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
trap cleanup EXIT

[ "$(id -u)" = 0 ] || die "run this as root: sudo sh $0"
case $user in
  root|'') die "JODD_AI_USER must not be root." ;;
  -*|[!a-z_]*|*[!a-z0-9_-]*) die "JODD_AI_USER '$user' is not a plain account name (lower-case letters, digits, - and _)." ;;
esac
work=$(mktemp -d) || die "cannot create a temp directory."

# --- bin/ and lib/ come from this checkout if we run from one (local_kit, found above), else the kit URL

# ---------------------------------------------------------------- 1. packages -----------
pm=
if command -v apt-get >/dev/null 2>&1; then pm=apt
elif command -v dnf >/dev/null 2>&1; then pm=dnf
elif command -v apk >/dev/null 2>&1; then pm=apk
fi
case $pm in
  apt)
    say "installing packages with apt-get"
    DEBIAN_FRONTEND=noninteractive; export DEBIAN_FRONTEND
    apt-get update -qq -o DPkg::Lock::Timeout=300 || warn "apt-get update failed; trying with the package lists already on this machine."
    # --force-confold: never replace an sshd_config the admin has edited.
    # --no-upgrade: do not upgrade packages the admin already has (openssh-server, bash, ...).
    # Lock::Timeout: a fresh VPS often has unattended-upgrades holding the dpkg lock.
    apt-get install -y -qq --no-upgrade -o DPkg::Lock::Timeout=300 -o Dpkg::Options::=--force-confold \
      openssh-server openssh-client bash curl ca-certificates procps util-linux coreutils >/dev/null \
      || die "apt-get could not install the packages (message above)." ;;
  dnf)
    say "installing packages with dnf"
    dnf -y -q install openssh-server openssh-clients bash curl ca-certificates procps-ng util-linux coreutils shadow-utils >/dev/null \
      || die "dnf could not install the packages (message above)." ;;
  apk)
    say "installing packages with apk"
    # Claude Code on Alpine (musl) needs libgcc, libstdc++ and the system ripgrep.
    base_pkgs="openssh bash curl ca-certificates procps util-linux coreutils libgcc libstdc++"
    # shellcheck disable=SC2086  # a list of package names
    apk add -q --no-cache $base_pkgs || die "apk could not install the packages (message above)."
    if ! apk add -q --no-cache ripgrep 2>/dev/null; then
      # ripgrep lives in the community repository, which a minimal repositories file may lack.
      if [ -f /etc/apk/repositories ] && ! grep -q '/community$' /etc/apk/repositories; then
        main_repo=$(grep '/main$' /etc/apk/repositories | head -n 1)
        [ -z "$main_repo" ] || echo "${main_repo%/main}/community" >> /etc/apk/repositories
      fi
      apk add -q --no-cache ripgrep || die "apk could not install ripgrep (Claude Code needs it on Alpine)."
    fi ;;
  *)
    warn "Unsupported package manager (this installer knows apt, dnf and apk)."
    warn "Follow the manual steps: $manual_url#other-linux"
    exit 1 ;;
esac

sshd_bin() {
  for _d in "$(command -v sshd 2>/dev/null || :)" /usr/sbin/sshd /usr/local/sbin/sshd /sbin/sshd; do
    [ -n "$_d" ] && [ -x "$_d" ] && { printf '%s\n' "$_d"; return 0; }
  done
  return 1
}
sshd=$(sshd_bin) || die "sshd is not installed even after the package step."
[ -d /run/sshd ] || [ ! -d /run ] || mkdir -p /run/sshd   # Debian's sshd -t wants it; systemd also creates it
mkdir -p /etc/ssh

if [ -n "${JODD_AI_SEED_CONFIG:-}" ]; then
  say "(test) appending ${JODD_AI_SEED_CONFIG} to $sshd_config"
  cat "$JODD_AI_SEED_CONFIG" >> "$sshd_config"
fi

# ---------------------------------------------------------------- helper files ----------
# Fetched/copied before the account steps so everything below can use render_dropin.
fetch() { # $1 path under the kit, $2 destination
  if [ -n "$local_kit" ]; then
    cp "$local_kit/$1" "$2" || die "cannot copy $local_kit/$1."
  else
    curl --proto '=https' --tlsv1.2 -fsSL "$kit_url/$1" -o "$2" || die "cannot download $kit_url/$1 (no network?). Download the kit and run install/linux.sh from it."
  fi
  if [ ! -s "$2" ] || ! sh -n "$2"; then die "$1 from the kit is empty or not a shell script; nothing was installed."; fi
}
mkdir -p "$work/kit/bin" "$work/kit/lib"
fetch bin/jodd-ai-host "$work/kit/bin/jodd-ai-host"
fetch lib/dropin.sh "$work/kit/lib/dropin.sh"
fetch lib/doctor.sh "$work/kit/lib/doctor.sh"
mkdir -p "$libdir" /usr/local/bin
cp "$work/kit/lib/dropin.sh" "$work/kit/lib/doctor.sh" "$libdir/" && chmod 644 "$libdir/dropin.sh" "$libdir/doctor.sh"
cp "$work/kit/bin/jodd-ai-host" "$helper.new" && chmod 755 "$helper.new" && mv "$helper.new" "$helper"
# shellcheck disable=SC1091
. "$libdir/dropin.sh"

# ---------------------------------------------------------------- 2. account ------------
if id "$user" >/dev/null 2>&1; then
  uid=$(id -u "$user")
  [ "$uid" -ge 1000 ] || die "$user is a system account (uid $uid); choose another JODD_AI_USER."
  # Jodd runs AI CLIs as this account, driven remotely: it must not be able to become root.
  for _g in $(id -nG "$user" 2>/dev/null); do
    case $_g in sudo|wheel|admin) die "$user is in the '$_g' group, i.e. it can act as root. Jodd runs AI CLIs as this account, so use an ordinary account: set JODD_AI_USER to a name that does not exist yet (or remove $user from '$_g')." ;; esac
  done
  say "using the existing account $user"
else
  say "creating the account $user"
  if [ "$pm" = apk ]; then
    adduser -D -s /bin/bash "$user" >/dev/null || die "adduser failed."
  else
    useradd -m -s /bin/bash "$user" || die "useradd failed."
  fi
fi
home=$(awk -F: -v u="$user" '$1 == u { print $6 }' /etc/passwd)
if [ -z "$home" ] || [ ! -d "$home" ]; then die "the home directory of $user ($home) does not exist."; fi
cur_shell=$(awk -F: -v u="$user" '$1 == u { print $7 }' /etc/passwd)
case $cur_shell in
  ''|*/nologin|*/false)
    bash_path=$(command -v bash) || die "bash is not installed."
    if command -v usermod >/dev/null 2>&1; then usermod -s "$bash_path" "$user"
    else sed "s|^\($user:.*:\)[^:]*\$|\1$bash_path|" /etc/passwd > "$work/passwd" && cat "$work/passwd" > /etc/passwd; fi ;;
esac
# The login PATH. bash reads ~/.bash_profile when it exists and ~/.profile otherwise, so the
# line goes into ~/.profile, and into ~/.bash_profile only if the account already has one
# (creating one would make bash stop reading ~/.profile).
# Everything is written AS THE ACCOUNT, never as root: the account (which runs remote-driven AI
# CLIs) controls these files, and a root `>>` or `chown` would follow a symlink it planted
# (~/.profile -> /usr/local/bin/jodd-ai-host) and hand it a file that root later runs.
sq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
# No `-l`: a login shell would run the account's own ~/.profile on root's terminal for nothing.
# `env -i`: su keeps root's environment otherwise, and the account can read its child's
# environment (/proc/PID/environ), e.g. a root API key or proxy password.
as_account() { env -i PATH=/usr/bin:/bin HOME="$home" "${JODD_AI_SU:-su}" -s /bin/sh -c "$1" "$user"; }
# shellcheck disable=SC2016  # the $ is for the account's shell
path_line='export PATH="$HOME/.local/bin:$HOME/.codex/bin:$PATH"'
add_path() {
  # shellcheck disable=SC2016  # the $ are for the account's shell
  _s=$(printf 'f=%s; line=%s; grep -qsxF "$line" "$f" || printf "%%s\\n" "$line" >> "$f"' "$(sq "$1")" "$(sq "$path_line")")
  as_account "$_s" || warn "could not add ~/.local/bin to $1 as $user (is it a link or read-only?). Check: sudo jodd-ai-host doctor"
}
add_path "$home/.profile"
[ ! -f "$home/.bash_profile" ] || add_path "$home/.bash_profile"
if [ "$pm" = apk ]; then
  # Alpine: Claude Code must use the system ripgrep.
  # shellcheck disable=SC2016
  _s=$(printf 'd=%s; f="$d/settings.json"; [ -e "$f" ] || { mkdir -p "$d" && printf "%%s\\n" %s > "$f"; }' "$(sq "$home/.claude")" "$(sq '{"env":{"USE_BUILTIN_RIPGREP":"0"}}')")
  as_account "$_s" || warn "could not write ~/.claude/settings.json as $user; Claude Code on Alpine needs USE_BUILTIN_RIPGREP=0."
fi

# ---------------------------------------------------------------- 3. password ----------
has_password() {
  _h=$(awk -F: -v u="$user" '$1 == u { print $2 }' /etc/shadow 2>/dev/null)
  case $_h in ''|'!'*|'*'*) return 1 ;; *) return 0 ;; esac
}
set_password() { printf '%s:%s\n' "$user" "$1" | chpasswd || die "chpasswd failed; the password of $user was not set."; }
mkdir -p "$state" && chmod 700 "$state"
envfile=$state/env-password.sha256
pw_given=${JODD_AI_PASSWORD:-}
case $pw_given in
  *'
'*) die "JODD_AI_PASSWORD contains a line break. Use a single-line password." ;;
esac
if [ -z "$pw_given" ] && ! has_password; then
  if [ -t 0 ]; then
    tries=0
    while [ -z "$pw_given" ] && [ "$tries" -lt 3 ]; do
      tries=$((tries + 1))
      printf 'Password for %s (used once, for Jodd'"'"'s Setup; empty = generate one): ' "$user" >&2
      stty -echo 2>/dev/null || :
      read -r p1 || p1=
      stty echo 2>/dev/null || :; echo >&2
      [ -n "$p1" ] || break
      printf 'Again: ' >&2
      stty -echo 2>/dev/null || :
      read -r p2 || p2=
      stty echo 2>/dev/null || :; echo >&2
      if [ "$p1" = "$p2" ]; then pw_given=$p1; else warn "the two passwords differ."; fi
    done
  fi
  if [ -z "$pw_given" ]; then
    pw_given=$(head -c 18 /dev/urandom | base64 | tr -d '/+=\n' | head -c 20)
    set_password "$pw_given"
    # Shown the moment it exists (the CLI downloads below take minutes), and again in the summary.
    generated=$pw_given
    say "generated password for $user: $generated"
  else
    set_password "$pw_given"
    say "password set for $user"
  fi
elif [ -n "$pw_given" ]; then
  # Applied when the account has none yet, or when the value differs from the one last applied
  # from the environment (a salted hash is kept root-only in $state). A password the admin
  # changed by hand is never reset by a re-run that does not pass a new JODD_AI_PASSWORD.
  old=$(cat "$envfile" 2>/dev/null || :)
  salt=${old%%:*}
  [ -n "$old" ] || salt=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
  hash=$(printf '%s%s' "$salt" "$pw_given" | sha256sum | cut -d' ' -f1)
  if ! has_password || [ "$old" != "$salt:$hash" ]; then
    set_password "$pw_given"
    ( umask 077; printf '%s:%s' "$salt" "$hash" > "$envfile" )
    say "password set for $user"
  else
    say "password of $user unchanged (JODD_AI_PASSWORD is the one already applied)"
  fi
else
  say "password of $user left as it is"
fi
unset pw_given

# ---------------------------------------------------------------- 4. host keys ---------
ssh-keygen -A >/dev/null 2>&1 || die "ssh-keygen -A failed; sshd has no host keys."
hostkey=/etc/ssh/ssh_host_ed25519_key.pub
[ -r "$hostkey" ] || die "no ed25519 host key at $hostkey (ssh-keygen -A did not create one)."

# ---------------------------------------------------------------- 5. sshd drop-in --------
# Keep the lock an admin set: a re-run must not turn password login back on.
locked=no
if grep -qs 'PasswordAuthentication no' "$dropin"; then locked=yes; fi
if [ "$locked" = yes ]; then want=no; else want=yes; fi

# What every OTHER account gets today; compared after the change.
probe() { # $1 user -> sshd's effective settings for that login, without Include lines
  "$sshd" -T -C "user=$1,host=x,addr=127.0.0.1" 2>/dev/null | grep -vi '^include ' || return 1
}
probe_users="root jodd-kit-probe-nobody"
# ... and the admin who ran the installer (their own login must not change either)
case ${SUDO_USER:-} in ''|root|"$user"|*[!a-zA-Z0-9._-]*) ;; *) probe_users="$probe_users $SUDO_USER" ;; esac
for u in $probe_users; do
  probe "$u" > "$work/base.$u" 2>/dev/null || : > "$work/base.$u.bad"
done

# Snapshot everything we may touch, so a failure restores exactly what was there.
main_edited=0 had_dropin=0 made_dir=0
cp -p "$sshd_config" "$work/main.orig" || die "cannot read $sshd_config."
if [ -f "$dropin" ]; then had_dropin=1; cp -p "$dropin" "$work/dropin.orig"; fi
[ -d "$dropin_dir" ] || made_dir=1

rollback() {
  config_pending=0
  if [ "$main_edited" = 1 ]; then cp -p "$work/main.orig" "$sshd_config"; fi
  if [ "$had_dropin" = 1 ]; then cp -p "$work/dropin.orig" "$dropin"; else rm -f "$dropin"; fi
  if [ "$made_dir" = 1 ]; then rmdir "$dropin_dir" 2>/dev/null || :; fi
}
abort_config() { # $1 = what failed
  rollback
  warn "$1"
  warn "The sshd configuration was restored to exactly what it was ($sshd_config and $dropin); the account and the helper stay."
  if ! "$sshd" -t >/dev/null 2>&1; then
    warn "Note: sshd rejects the configuration even in its original state (sshd -t, above). Fix that first, then run this installer again."
  fi
  exit 1
}

config_pending=1   # from here an interrupt rolls the sshd configuration back (see cleanup)
mkdir -p "$dropin_dir"
render_dropin "$user" "$locked" > "$work/dropin.new"
if ! cmp -s "$work/dropin.new" "$dropin" 2>/dev/null; then
  tmp=$(mktemp "$dropin_dir/.jodd-dropin.XXXXXX") || die "cannot create a temp file in $dropin_dir; nothing was changed."
  { cat "$work/dropin.new" > "$tmp" && chmod 644 "$tmp" && mv "$tmp" "$dropin"; } \
    || { rm -f "$tmp"; rollback; die "cannot write $dropin; nothing was changed."; }
fi

# sshd's first value wins and an Include after a global `Match` is swallowed by it, so the
# drop-in must be read before anything else. Only OUR file is included at the top: that
# cannot change another account's settings, unlike hoisting the whole sshd_config.d/*.conf.
first_line=$(grep -Ev '^[[:space:]]*(#|$)' "$sshd_config" | head -n 1 || :)
if ! printf '%s\n' "$first_line" | grep -Eiq '^[[:space:]]*Include[[:space:]]+"?(/etc/ssh/)?sshd_config\.d/(\*(\.conf)?|50-jodd-ai\.conf)"?([[:space:]]|$)'; then
  say "adding 'Include $dropin' at the top of $sshd_config"
  main_edited=1
  tmp=$(mktemp "$(dirname "$sshd_config")/.jodd-sshd_config.XXXXXX") || { rollback; die "cannot create a temp file next to $sshd_config; nothing was changed."; }
  if cp -p "$sshd_config" "$tmp" \
     && { printf '%s\n%s\n' '# Added by the Jodd AI host kit: read first, so a Match block below cannot swallow it.' "Include $dropin"; cat "$work/main.orig"; } > "$tmp" \
     && mv "$tmp" "$sshd_config"; then
    :
  else
    rm -f "$tmp"; rollback; die "cannot edit $sshd_config; nothing was changed."
  fi
fi
if [ "${JODD_AI_TEST_INTERRUPT:-}" = after-config-write ]; then kill -INT $$; fi

if ! out=$("$sshd" -t 2>&1); then
  abort_config "sshd rejected the new configuration:
$out"
fi
eff=$("$sshd" -T -C "user=$user,host=x,addr=127.0.0.1" 2>&1) || abort_config "sshd -T failed:
$eff"
got=$(printf '%s\n' "$eff" | awk '$1 == "passwordauthentication" { print $2; exit }')
[ "$got" = "$want" ] || abort_config "sshd would not apply the setting: password login for $user is '${got:-unknown}', expected '$want'. A rule in $sshd_config probably overrides it."
for u in $probe_users; do
  [ ! -e "$work/base.$u.bad" ] || continue
  probe "$u" > "$work/now.$u" 2>/dev/null || : > "$work/now.$u"
  if ! cmp -s "$work/base.$u" "$work/now.$u"; then
    abort_config "the change would alter the effective sshd settings of other accounts ($u):
$(diff "$work/base.$u" "$work/now.$u" | head -n 10)"
  fi
done
config_pending=0
say "sshd accepts the configuration; password login: $user $want, every other account unchanged"
# The drop-in sets PasswordAuthentication only. A global `AuthenticationMethods publickey`, or an
# AllowUsers / AllowGroups / DenyUsers / DenyGroups that leaves the account out, still refuses it -
# and those are the admin's: say which one to edit, change nothing, do not fail the install.
problems=$(printf '%s\n' "$eff" | sshd_login_problems "$user" "$(id -Gn "$user" 2>/dev/null || :)")
if [ -n "$problems" ]; then
  warn "WARNING: another sshd setting keeps $user out, and the installer leaves it alone (it is yours):"
  printf '%s\n' "$problems" | sed 's/^/    - /' >&2
  warn "Jodd's Setup fails until it is changed. Then check with: sudo jodd-ai-host doctor"
fi

# ---------------------------------------------------------------- helper config ---------
port=$("$sshd" -T 2>/dev/null | awk '$1 == "port" { print $2; exit }')
port=${port:-22}
cat > "$work/conf" <<EOC
JODD_AI_USER=$user
JODD_AI_PORT=$port
JODD_AI_KIND=linux
JODD_AI_DROPIN=$dropin
JODD_AI_HOSTKEY=$hostkey
JODD_AI_LIB=$libdir
EOC
cp "$work/conf" /etc/jodd-ai-host.conf.new && chmod 644 /etc/jodd-ai-host.conf.new && mv /etc/jodd-ai-host.conf.new /etc/jodd-ai-host.conf

# ---------------------------------------------------------------- 6. CLIs ---------------
cli_ok=1
if [ "${JODD_AI_SKIP_CLIS:-}" = 1 ]; then
  say "skipping the CLI install (JODD_AI_SKIP_CLIS=1). Later: sudo jodd-ai-host update-clis"
else
  say "installing claude and codex for $user (official installers; this can take a few minutes)"
  if ! "$helper" update-clis; then
    cli_ok=0
    warn "At least one CLI did not install (no network?). Everything else is set up. Retry later: sudo jodd-ai-host update-clis"
  fi
fi

# ---------------------------------------------------------------- 7. sshd ---------------
sshd_running() { pgrep -x sshd >/dev/null 2>&1; }
# The listener's pid is in sshd's pidfile; failing that it is the oldest sshd process.
hup_listener() {
  _pf=$("$sshd" -T 2>/dev/null | awk '$1 == "pidfile" { print $2; exit }')
  _p=$(cat "${_pf:-/run/sshd.pid}" 2>/dev/null || :)
  case $_p in ''|*[!0-9]*) _p= ;; esac
  if [ -n "$_p" ] && kill -0 "$_p" 2>/dev/null && { [ ! -r "/proc/$_p/comm" ] || [ "$(cat "/proc/$_p/comm" 2>/dev/null)" = sshd ]; }; then
    kill -HUP "$_p"
  else
    pkill -HUP -o -x sshd
  fi
}
start_sshd() {
  if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    unit=sshd
    systemctl cat ssh.service >/dev/null 2>&1 && unit=ssh
    systemctl enable "$unit" >/dev/null 2>&1 || :
    if systemctl is-active --quiet "$unit"; then systemctl reload "$unit" || systemctl restart "$unit"; else systemctl start "$unit"; fi
    return
  fi
  if command -v rc-service >/dev/null 2>&1 && [ -d /run/openrc ]; then
    rc-update add sshd default >/dev/null 2>&1 || :
    if rc-service sshd status >/dev/null 2>&1; then rc-service sshd reload; else rc-service sshd start; fi
    return
  fi
  # Reload, never `pkill -HUP -x sshd`: before OpenSSH 9.8 that also kills every open session
  # (the per-connection processes are named sshd too), the admin's own included. The init script
  # signals the listener only; failing that, we signal the listener ourselves.
  if sshd_running; then
    if command -v service >/dev/null 2>&1 && { service ssh reload || service sshd reload; } >/dev/null 2>&1; then return; fi
    hup_listener && return
  fi
  if command -v service >/dev/null 2>&1 && { service ssh restart || service sshd restart; } >/dev/null 2>&1 && sshd_running; then return; fi
  if [ "${JODD_AI_START_SSHD:-}" = 1 ]; then "$sshd"; return; fi
  return 1
}
if start_sshd; then
  say "sshd is running with the new configuration"
else
  warn "No init system found to start sshd. Start it yourself (e.g. $sshd), then run: sudo jodd-ai-host doctor"
fi

# ---------------------------------------------------------------- 8. summary ------------
echo
"$helper" doctor || :
echo
fp=$("$helper" fingerprint 2>/dev/null || echo "(unreadable - run: sudo jodd-ai-host doctor)")
addrs=$(hostname -I 2>/dev/null || :)
[ -n "$addrs" ] || addrs=$(ip -4 -o addr show scope global 2>/dev/null | awk '{ sub(/\/.*/, "", $4); printf "%s ", $4 }' || :)
addrs=$(printf '%s' "$addrs" | sed 's/[[:space:]]*$//')
[ -n "$addrs" ] || addrs="(this server's address)"
shown_port=${JODD_AI_PORT:-$port}
cat <<EOS
------------------------------------------------------------------------
Jodd AI host is ready.
  Host:         $addrs
  Port:         $shown_port
  Account:      $user
  Fingerprint:  $fp   (Jodd shows the same text; compare before you trust it)
EOS
[ -z "$generated" ] || echo "  Password:     $generated   (generated; Jodd asks for it once, in Setup)"
cat <<EOS

Next:
  1. Sign the CLIs in, as your own account:
       sudo jodd-ai-host signin claude
       sudo jodd-ai-host signin codex
  2. In Jodd: App settings -> LLM provider -> Agent CLI (headless) -> Runs on: A server
     (or Account settings -> LLM Provider, for one account's own provider).
     Enter the host, port $shown_port, user $user and the password; confirm the fingerprint.
  3. After Setup:  sudo jodd-ai-host lock     (before an Update in Jodd:  sudo jodd-ai-host unlock)
EOS
if [ "$locked" = yes ]; then
  echo
  echo "Password login for $user is LOCKED (kept from before). Before Setup in Jodd: sudo jodd-ai-host unlock"
fi
if [ "$cli_ok" = 0 ]; then
  echo
  echo "claude and/or codex are not installed yet: sudo jodd-ai-host update-clis"
fi
