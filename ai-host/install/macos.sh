#!/bin/sh
# Jodd AI host kit - macOS installer (macOS 13 or newer; for a Mac that stays on).
#
# Prepares this Mac so Jodd can run YOUR OWN claude / codex CLI on it, signed in with YOUR OWN
# account, over a restricted SSH key. Read it before you run it - and try it first, it changes
# nothing with --dry-run (no sudo needed):
#
#   curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host/install/macos.sh -o jodd-ai-host-install.sh
#   sh jodd-ai-host-install.sh --dry-run
#   sudo sh jodd-ai-host-install.sh
#
# What it does, in order (re-running repairs instead of duplicating):
#   1. creates a standard, hidden account (zsh login shell, ~/.local/bin on its login PATH);
#      asks you for its password - the password is only for Jodd's one-time Setup;
#      afterwards `sudo jodd-ai-host lock` turns it off. The account gets a group of its own
#      instead of `staff` (which can enter every home folder on a Mac), and its own home
#      folder is closed (700); the summary lists other home folders that are still open
#   2. creates missing SSH host keys (never replaces existing ones)
#   3. writes /etc/ssh/sshd_config.d/50-jodd-ai.conf, a `Match User <account>` block that
#      allows password login for THAT account only, and makes sure sshd_config reads it
#      ahead of any Match block of yours. It is validated with `sshd -t`, and the effective
#      settings of other accounts are compared before and after; if anything is off,
#      sshd_config and the drop-in are restored byte for byte and nothing else is changed
#   4. turns Remote Login on and lets the account in (System Settings -> General -> Sharing).
#      macOS only lets a terminal do this with Full Disk Access; if yours has none, the
#      installer tells you the three clicks and waits
#   5. installs claude and codex as the account, from their official installers
#   6. installs /usr/local/bin/jodd-ai-host and prints what to do next
#
# Options:
#   --dry-run   print every action (including the exact sshd drop-in text and the line added to
#               sshd_config) and change nothing. Needs no sudo; it only reads this Mac.
#   --ssh-only-ai   (unattended runs) if Remote Login is off and its allow-list also names other
#               users or groups, remove them from it first, so only the AI account gets ssh
#   --ssh-keep-list (unattended runs) the opposite: keep that list as it is. With neither flag,
#               a run on a terminal asks; a run without one stops before turning Remote Login on.
#   --no-sleep  also run `pmset -c sleep 0 disksleep 0`, so a Mac on its charger never sleeps
#               (without it the installer only tells you how to do it in System Settings)
#   -h, --help  this text
#
# Environment:
#   JODD_AI_USER      account name (default jodd-ai)
#   JODD_AI_PASSWORD  its password, for a run with no terminal. Without it you are asked, or -
#                     with no terminal - one is generated and printed. It is passed to
#                     sysadminctl as an argument, so another user of this Mac could see it in
#                     `ps` for a moment; the prompt does not have that problem. An existing
#                     account's password is never changed (sudo jodd-ai-host password).
#   JODD_AI_PORT      only what the final summary reports (e.g. a forwarded port)
#   JODD_AI_KIT_URL   where bin/ and lib/ are fetched from when this script is not run
#                     from a checkout of the kit
#                     (default https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host)
#   JODD_AI_SKIP_CLIS=1   do not install claude / codex (offline runs; `jodd-ai-host update-clis` later)
#
# Test-only knob (the same one linux.sh has; used by the live-run checklist, never needed otherwise):
#   JODD_AI_TEST_INTERRUPT=after-config-write
#                     send this script an INT right after it wrote the sshd config and
#                     before it validated it, to prove the interrupt rolls the config back
#
# Needs root (except --dry-run). POSIX sh.
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
fullname="Jodd AI host"
kit_url=${JODD_AI_KIT_URL:-https://raw.githubusercontent.com/BBM-Co-ORG/Jodd-public/main/ai-host}
sshd=/usr/sbin/sshd
sshd_config=/etc/ssh/sshd_config
dropin_dir=/etc/ssh/sshd_config.d
dropin=$dropin_dir/50-jodd-ai.conf
libdir=/usr/local/lib/jodd-ai-host
helper=/usr/local/bin/jodd-ai-host
hostkey=/etc/ssh/ssh_host_ed25519_key.pub
ssh_group=com.apple.access_ssh
staff_gid=20         # macOS's `staff`: every account sysadminctl creates gets it as its primary group
users_dir=/Users     # where the summary looks for other people's home folders
manual_url=https://github.com/BBM-Co-ORG/Jodd-public/blob/main/docs/ai-host/INSTALL.md

say()  { printf '%s\n' "jodd-ai-host: $*"; }
warn() { printf '%s\n' "jodd-ai-host: $*" >&2; }
die()  { warn "$*"; exit 1; }

dry=0 nosleep=0 ssh_choice=
for _a in "$@"; do
  case $_a in
    --dry-run) dry=1 ;;
    --ssh-only-ai) [ -z "$ssh_choice" ] || [ "$ssh_choice" = only-ai ] || die "--ssh-only-ai and --ssh-keep-list exclude each other."; ssh_choice=only-ai ;;
    --ssh-keep-list) [ -z "$ssh_choice" ] || [ "$ssh_choice" = keep ] || die "--ssh-only-ai and --ssh-keep-list exclude each other."; ssh_choice=keep ;;
    --no-sleep) nosleep=1 ;;
    -h|--help) sed -n '2,/^set -eu$/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//' ; exit 0 ;;
    *) die "unknown option '$_a' (known: --dry-run, --no-sleep, --ssh-only-ai, --ssh-keep-list, --help)." ;;
  esac
done
# plan: what a dry run prints instead of doing something.
plan() { printf '%s\n' "  [dry-run] $*"; }
if [ "$dry" = 1 ]; then say "DRY RUN: nothing below is done; this Mac is only read."; fi

work=
generated=
config_pending=0   # 1 from the first write to sshd's config until it has been validated
made_group=0       # 1 while we created $ssh_group (cleanup removes it again unless Remote Login was attempted)
rl_attempted=0     # 1 from the moment before systemsetup is run: from then on the allow-list stays as it is
removed_members="" # "type:name" lines taken out of $ssh_group on the user's say-so (cleanup puts them back)
cleanup() {
  _rc=$?
  # Ctrl-C at the password prompt must not leave the terminal without echo.
  if [ -t 0 ]; then stty echo 2>/dev/null || :; fi
  # An interrupt between writing the sshd config and validating it: put back what was there.
  if [ "$config_pending" = 1 ]; then
    rollback
    warn "Interrupted: the sshd configuration was restored to exactly what it was."
  fi
  # A run that failed BEFORE Remote Login was attempted puts the allow-list back as it found it:
  # the group we created goes again, members we took out come back. After the attempt it stays
  # (a list with only the AI account does nothing while Remote Login is off, and it keeps a
  # later manual switch-on from opening ssh to everyone).
  if [ "$rl_attempted" = 0 ]; then
    if [ "$made_group" = 1 ]; then dseditgroup -o delete -q "$ssh_group" >/dev/null 2>&1 || :; fi
    if [ -n "$removed_members" ]; then restore_removed; fi
  fi
  if [ "$dry" = 0 ]; then rm -f "$dropin_dir"/.jodd-dropin.* "$(dirname "$sshd_config")"/.jodd-sshd_config.* 2>/dev/null || :; fi
  if [ -n "$work" ]; then rm -rf "$work"; fi
  return $_rc
}
# Turn the signals into exits so the EXIT trap (and the rollback) always runs.
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
trap cleanup EXIT

[ "$(uname -s)" = Darwin ] || die "this installer is for macOS. On Linux use install/linux.sh."
if [ "$dry" = 0 ]; then
  [ "$(id -u)" = 0 ] || die "run this as root: sudo sh $0   (or try it first: sh $0 --dry-run)"
fi
case $user in
  root|'') die "JODD_AI_USER must not be root." ;;
  -*|[!a-z_]*|*[!a-z0-9_-]*) die "JODD_AI_USER '$user' is not a plain account name (lower-case letters, digits, - and _)." ;;
esac

# ---------------------------------------------------------------- 0. macOS version ------
macos_version=$(sw_vers -productVersion 2>/dev/null) || die "cannot read the macOS version (sw_vers)."
macos_major=${macos_version%%.*}
case $macos_major in ''|*[!0-9]*) die "cannot understand the macOS version '$macos_version'." ;; esac
[ "$macos_major" -ge 13 ] || die "macOS $macos_version is too old: the AI host kit needs macOS 13 (Ventura) or newer."
say "macOS $macos_version"
[ -x "$sshd" ] || die "$sshd is missing; this does not look like a normal macOS."

work=$(mktemp -d) || die "cannot create a temp directory."

# --- bin/ and lib/ come from this checkout if we run from one (local_kit, found above), else the kit URL

# ---------------------------------------------------------------- helper files ----------
# Fetched/copied first so everything below can use render_dropin. A dry run fetches them too (a
# read), so it can print the exact drop-in; it installs nothing.
fetch() { # $1 path under the kit, $2 destination
  if [ -n "$local_kit" ]; then
    cp "$local_kit/$1" "$2" || die "cannot copy $local_kit/$1."
  else
    curl --proto '=https' --tlsv1.2 -fsSL "$kit_url/$1" -o "$2" || die "cannot download $kit_url/$1 (no network?). Download the kit and run install/macos.sh from it."
  fi
  if [ ! -s "$2" ] || ! sh -n "$2"; then die "$1 from the kit is empty or not a shell script; nothing was installed."; fi
}
mkdir -p "$work/kit/bin" "$work/kit/lib"
fetch bin/jodd-ai-host "$work/kit/bin/jodd-ai-host"
fetch lib/dropin.sh "$work/kit/lib/dropin.sh"
fetch lib/doctor.sh "$work/kit/lib/doctor.sh"
if [ "$dry" = 1 ]; then
  plan "install $helper and $libdir/{dropin,doctor}.sh (from ${local_kit:-$kit_url})"
  # shellcheck disable=SC1091
  . "$work/kit/lib/dropin.sh"
else
  mkdir -p "$libdir" /usr/local/bin
  cp "$work/kit/lib/dropin.sh" "$work/kit/lib/doctor.sh" "$libdir/" && chmod 644 "$libdir/dropin.sh" "$libdir/doctor.sh"
  cp "$work/kit/bin/jodd-ai-host" "$helper.new" && chmod 755 "$helper.new" && mv "$helper.new" "$helper"
  # shellcheck disable=SC1091
  . "$libdir/dropin.sh"
fi

# ---------------------------------------------------------------- 1. account ------------
dsread() { dscl . -read "/Users/$user" "$1" 2>/dev/null | awk 'NR == 1 { print $2 }'; }
have_account=0
if dscl . -read "/Users/$user" UniqueID >/dev/null 2>&1; then have_account=1; fi
if [ "$have_account" = 1 ]; then
  uid=$(dsread UniqueID)
  case $uid in ''|*[!0-9]*) die "cannot read the user id of $user." ;; esac
  [ "$uid" -ge 500 ] || die "$user is a system account (uid $uid); choose another JODD_AI_USER."
  # Jodd runs AI CLIs as this account, driven remotely: it must not be an administrator.
  for _g in $(id -Gn "$user" 2>/dev/null); do
    [ "$_g" != admin ] || die "$user is an administrator (member of the 'admin' group). Jodd runs AI CLIs as this account, so use a standard account: set JODD_AI_USER to a name that does not exist yet (or make $user a standard user in System Settings -> Users & Groups)."
  done
  say "using the existing account $user (password and visibility left as they are)"
else
  if [ "$dry" = 1 ]; then
    if [ -n "${JODD_AI_PASSWORD:-}" ]; then pwhow="from JODD_AI_PASSWORD"
    elif [ -t 0 ]; then pwhow="sysadminctl asks you for it"
    else pwhow="generated and printed (no terminal to ask on)"; fi
    plan "create the account: sysadminctl -addUser $user -fullName \"$fullname\" -shell /bin/zsh -password ...   (password: $pwhow)"
    plan "hide it from the login window: dscl . create /Users/$user IsHidden 1"
  else
    pw_given=${JODD_AI_PASSWORD:-}
    case $pw_given in
      *'
'*) die "JODD_AI_PASSWORD contains a line break. Use a single-line password." ;;
    esac
    say "creating the account $user"
    if [ -n "$pw_given" ]; then
      sysadminctl -addUser "$user" -fullName "$fullname" -shell /bin/zsh -password "$pw_given" || die "sysadminctl could not create the account (message above)."
    elif [ -t 0 ]; then
      # sysadminctl asks for the password itself, so it is never on a command line.
      sysadminctl -addUser "$user" -fullName "$fullname" -shell /bin/zsh -password - || die "sysadminctl could not create the account (message above)."
    else
      pw_given=$(head -c 18 /dev/urandom | base64 | tr -d '/+=\n' | head -c 20)
      sysadminctl -addUser "$user" -fullName "$fullname" -shell /bin/zsh -password "$pw_given" || die "sysadminctl could not create the account (message above)."
      # Shown the moment it exists (the CLI downloads below take minutes), and again in the summary.
      generated=$pw_given
      say "generated password for $user: $generated"
    fi
    unset pw_given
    dscl . -read "/Users/$user" UniqueID >/dev/null 2>&1 || die "the account $user does not exist after sysadminctl (message above)."
    dscl . create "/Users/$user" IsHidden 1 || warn "could not hide $user from the login window (harmless)."
    uid=$(dsread UniqueID)
  fi
fi

if [ "$dry" = 1 ]; then
  home=$(dsread NFSHomeDirectory); home=${home:-/Users/$user}
  cur_shell=$(dsread UserShell)
else
  home=$(dsread NFSHomeDirectory)
  if [ -z "$home" ] || [ ! -d "$home" ]; then
    createhomedir -c -u "$user" >/dev/null 2>&1 || :
    home=$(dsread NFSHomeDirectory)
  fi
  if [ -z "$home" ] || [ ! -d "$home" ]; then die "the home directory of $user (${home:-none}) does not exist."; fi
  cur_shell=$(dsread UserShell)
fi
case $cur_shell in
  ''|*/nologin|*/false)
    if [ "$have_account" = 1 ]; then
      if [ "$dry" = 1 ]; then plan "give $user a login shell: dscl . -create /Users/$user UserShell /bin/zsh (it has '${cur_shell:-none}')"
      else dscl . -create "/Users/$user" UserShell /bin/zsh || die "cannot give $user a login shell."; fi
    fi ;;
esac

# ---------------------------------------------------------------- 1b. own group, closed home
# sysadminctl makes `staff` (gid 20) the account's primary group, like every account's. macOS
# homes are drwxr-x--- OWNER:staff, so a member of staff can enter every other home and read
# whatever in it is not closed (only Desktop, Documents, Downloads, Movies, Music, Pictures and Library are) - and Codex's
# read-only sandbox still reads. So the AI account gets a group of its own, and its own home is
# closed to the others in turn (it holds the CLIs' sign-ins).
own_group() { # make a group named like the account its primary group, unless it has one already
  _pg=$(dsread PrimaryGroupID)   # empty for an account a dry run has not created
  if [ -n "$_pg" ] && [ "$_pg" != "$staff_gid" ]; then
    if [ "$dry" = 1 ]; then plan "$user's primary group is $_pg, not staff: left as it is"
    else say "$user's primary group is $_pg, not staff: left as it is"; fi
    return 0
  fi
  _gid=$(dscl . -read "/Groups/$user" PrimaryGroupID 2>/dev/null | awk 'NR == 1 { print $2 }')
  if [ "$dry" = 1 ]; then
    if [ -z "$_gid" ]; then plan "give $user a group of its own: dseditgroup -o create -r \"$fullname\" $user"
    else plan "the group $user exists (gid $_gid); it becomes $user's own"; fi
    plan "make it $user's primary group instead of staff (gid $staff_gid): dscl . -create /Users/$user PrimaryGroupID <gid of the group $user>"
    plan "  (staff can enter every home folder on this Mac; the account must not be in it)"
    return 0
  fi
  if [ -z "$_gid" ]; then
    dseditgroup -o create -r "$fullname" "$user" || die "cannot create the group $user for the account (dseditgroup, message above). Run this installer again."
    _gid=$(dscl . -read "/Groups/$user" PrimaryGroupID 2>/dev/null | awk 'NR == 1 { print $2 }')
  fi
  case $_gid in ''|*[!0-9]*) die "cannot read the id of the group $user (dscl . -read /Groups/$user PrimaryGroupID)." ;; esac
  if [ "$_gid" = "$staff_gid" ] || [ "$_gid" -lt 500 ]; then die "the group $user has the id $_gid, which is not a group of its own. Remove it or choose another JODD_AI_USER."; fi
  dscl . -create "/Users/$user" PrimaryGroupID "$_gid" || die "cannot make $user's primary group $user ($_gid) (dscl, message above). Run this installer again."
  say "$user now has a group of its own ($user, gid $_gid) instead of staff"
}
close_home() { # chmod 700 the account's home folder - the folder itself, never anything inside it
  if [ "$dry" = 1 ]; then
    # shellcheck disable=SC2012  # only the mode string of one known folder
    if [ -d "$home" ] && [ "$(ls -ld "$home" | cut -c1-10)" = drwx------ ]; then plan "$home is already closed to other accounts (700)"
    else plan "close $home to other accounts: chmod 700 $home   (the folder itself only)"; fi
    return 0
  fi
  # Root acts on the folder itself only: never recursively inside a tree the account controls.
  if [ -L "$home" ]; then warn "$home is a symbolic link; not changing its mode. Close it yourself: chmod 700 on the folder it points to."; return 0; fi
  chmod 700 "$home" || warn "could not close $home (chmod 700); other accounts may read the CLIs' sign-ins in it."
}
in_staff=unknown
staff_check() { # after own_group: is the account really out of staff? Warn, never fail.
  if [ "$dry" = 1 ]; then
    plan "then check: dsmemberutil checkmembership -U $user -G staff (a WARNING if $user is still in staff)"
    return 0
  fi
  dsmemberutil flushcache >/dev/null 2>&1 || :
  _o=$(dsmemberutil checkmembership -U "$user" -G staff 2>&1 || :)
  case $_o in
    *"not a member"*) in_staff=0 ;;
    *"is a member"*)
      in_staff=1
      warn "WARNING: $user is still a member of the staff group (dsmemberutil: $_o). It can enter the home folder of every account in staff and read whatever in it is not closed. Close yours (see the summary): chmod 700 /Users/<you>" ;;
    *) warn "could not check whether $user is in the staff group (dsmemberutil: ${_o:-no output}). Check: dsmemberutil checkmembership -U $user -G staff" ;;
  esac
}
# Other accounts' home folders that staff or everyone can enter: "PATH staff" or "PATH everyone"
# per line. Read-only (the summary prints chmod advice; it changes nothing).
open_homes() {
  for _d in "$users_dir"/*; do
    if [ ! -d "$_d" ] || [ -L "$_d" ]; then continue; fi
    case ${_d##*/} in Shared|Guest|"$user") continue ;; esac
    [ "$_d" != "$home" ] || continue
    _ls=$(ls -lnd "$_d") || continue
    _m=$(printf '%s' "$_ls" | cut -c1-10); _g=$(printf '%s' "$_ls" | awk '{ print $4 }')
    case $_m in
      ?????????[xt]) printf '%s everyone\n' "$_d" ;;
      ??????[xs]???) [ "$_g" != "$staff_gid" ] || printf '%s staff\n' "$_d" ;;
    esac
  done
  return 0
}
open_homes_text() { # $1 = open_homes output
  [ -n "$1" ] || return 0
  if [ "$in_staff" = 1 ]; then
    echo "$user is STILL in the staff group, so it can enter these home folders and read whatever"
    echo "in them is not closed (on a Mac only Desktop, Documents, Downloads,"
    echo "Movies, Music, Pictures and Library are):"
  else
    echo "On a Mac every account you create is in the staff group, which can enter these home folders"
    echo "and read whatever in them is not closed (only Desktop, Documents,"
    echo "Downloads, Movies, Music, Pictures and Library are)."
    echo "$user has a group of its own, so of these it can enter only those open to everyone. Close them all:"
  fi
  printf '%s\n' "$1" | while IFS= read -r _ln; do
    _d=${_ln% *} _who=${_ln##* }   # the folder name may contain spaces; the last word is who
    if [ "$_who" = everyone ]; then _why="open to every account, $user included"; else _why="open to staff"; fi
    printf '    chmod 700 %s     (%s; run it as that user, or with sudo)\n' "$(sq "$_d")" "$_why"
  done
  echo "  Keep secrets (API keys, .env files) out of folders other accounts can read."
}
own_group
close_home
staff_check

# The login PATH. zsh (the default login shell) reads ~/.zprofile at login, sh/bash read ~/.profile;
# bash prefers ~/.bash_profile when it exists, so it is only extended where the account has one
# (creating one would make bash stop reading ~/.profile).
# shellcheck disable=SC2016  # the $ is for the account's shell
path_line='export PATH="$HOME/.local/bin:$HOME/.codex/bin:$PATH"'
# Written AS THE ACCOUNT, never as root: the account (which runs remote-driven AI CLIs) controls
# these files, and a root `>>` or `chown` would follow a symlink it planted
# (~/.profile -> /usr/local/bin/jodd-ai-host) and hand it a file that root later runs.
sq() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }
add_path() {
  # shellcheck disable=SC2016  # the $ are for the account's shell
  _s=$(printf 'f=%s; line=%s; grep -qsxF "$line" "$f" || printf "%%s\\n" "$line" >> "$f"' "$(sq "$1")" "$(sq "$path_line")")
  sudo -H -u "$user" /bin/sh -c "$_s" || warn "could not add ~/.local/bin to $1 as $user (is it a link or read-only?). Check: sudo jodd-ai-host doctor"
}
if [ "$dry" = 1 ]; then
  for _f in .zprofile .profile; do
    if grep -qsxF "$path_line" "$home/$_f"; then plan "$home/$_f already has the login PATH line"
    else plan "append to $home/$_f (written as $user, not as root):  $path_line"; fi
  done
  if [ -f "$home/.bash_profile" ]; then plan "append the same line to $home/.bash_profile, as $user (it exists)"; fi
else
  add_path "$home/.zprofile"
  add_path "$home/.profile"
  [ ! -f "$home/.bash_profile" ] || add_path "$home/.bash_profile"
fi

# ---------------------------------------------------------------- 2. host keys ---------
if [ "$dry" = 1 ]; then
  if [ -r "$hostkey" ]; then plan "SSH host keys: $hostkey exists; none are replaced"
  else plan "create the SSH host keys: ssh-keygen -A (macOS also creates them on the first connection)"; fi
else
  ssh-keygen -A >/dev/null 2>&1 || die "ssh-keygen -A failed; sshd has no host keys."
  [ -r "$hostkey" ] || die "no ed25519 host key at $hostkey (ssh-keygen -A did not create one)."
fi

# ---------------------------------------------------------------- 3. sshd drop-in --------
# Keep the lock an admin set: a re-run must not turn password login back on.
locked=no
if grep -qs 'PasswordAuthentication no' "$dropin"; then locked=yes; fi
if [ "$locked" = yes ]; then want=no; else want=yes; fi

# sshd's first value wins and an Include after a global `Match` is swallowed by it, so the
# drop-in must be read before anything else. Only OUR file is included at the top: that
# cannot change another account's settings, unlike hoisting the whole sshd_config.d/*.
# Apple's own sshd_config already starts with `Include /etc/ssh/sshd_config.d/*`, which counts.
first_line=$(grep -Ev '^[[:space:]]*(#|$)' "$sshd_config" | head -n 1 || :)
needs_include=1
if printf '%s\n' "$first_line" | grep -Eiq '^[[:space:]]*Include[[:space:]]+"?(/etc/ssh/)?sshd_config\.d/(\*(\.conf)?|50-jodd-ai\.conf)"?([[:space:]]|$)'; then needs_include=0; fi

if [ "$dry" = 1 ]; then
  plan "write $dropin (mode 644), exactly:"
  render_dropin "$user" "$locked" | sed 's/^/        | /'
  if [ "$locked" = yes ]; then plan "(password login is currently LOCKED in the existing drop-in; a re-run keeps it locked)"; fi
  if [ "$needs_include" = 1 ]; then
    plan "add these two lines at the very top of $sshd_config (its first setting is: ${first_line:-none}):"
    printf '%s\n' "        | # Added by the Jodd AI host kit: read first, so a Match block below cannot swallow it." "        | Include $dropin"
  else
    plan "$sshd_config already reads the drop-in first (its first setting is: $first_line); it is not edited"
  fi
  plan "before and after: compare sshd -T for root and an unknown user; validate with sshd -t and"
  plan "  sshd -T -C user=$user (must say passwordauthentication $want); on any difference restore both files byte for byte"
  plan "sshd needs no reload on macOS: launchd starts a fresh sshd for every connection, which reads the files again"
  plan "then check sshd -T -C user=$user for AuthenticationMethods (must allow 'password' alone) and AllowUsers/AllowGroups/DenyUsers/DenyGroups (must let $user in): a warning naming the setting if not; those are never changed"
else
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
  if [ "$needs_include" = 1 ]; then
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
  # No reload: macOS starts sshd from launchd for every connection (ssh.plist: inetdCompatibility,
  # Wait=false), so the next connection reads the files above. `launchctl kickstart -k` is not
  # used: it fails while no connection is open and, when one is, kills it.
fi

# ---------------------------------------------------------------- helper config ---------
# launchd listens on the "ssh" service (port 22); sshd_config's Port is not consulted.
port=22
if [ "$dry" = 1 ]; then
  plan "write /etc/jodd-ai-host.conf:"
  printf '%s\n' "        | JODD_AI_USER=$user" "        | JODD_AI_PORT=$port" "        | JODD_AI_KIND=macos" \
    "        | JODD_AI_DROPIN=$dropin" "        | JODD_AI_HOSTKEY=$hostkey" "        | JODD_AI_LIB=$libdir"
else
  cat > "$work/conf" <<EOC
JODD_AI_USER=$user
JODD_AI_PORT=$port
JODD_AI_KIND=macos
JODD_AI_DROPIN=$dropin
JODD_AI_HOSTKEY=$hostkey
JODD_AI_LIB=$libdir
EOC
  cp "$work/conf" /etc/jodd-ai-host.conf.new && chmod 644 /etc/jodd-ai-host.conf.new && mv /etc/jodd-ai-host.conf.new /etc/jodd-ai-host.conf
fi

# ---------------------------------------------------------------- 4. Remote Login -------
# Remote Login is on when launchd has the sshd service loaded (or has it enabled). Not "something
# answers on port 22": any other service there would pass that.
remote_login_on() {
  launchctl print system/com.openssh.sshd >/dev/null 2>&1 && return 0
  launchctl print-disabled system 2>/dev/null | grep -Eq '"com\.openssh\.sshd"[[:space:]]*=>[[:space:]]*(enabled|false)'
}
group_exists() { dseditgroup -o read "$ssh_group" >/dev/null 2>&1; }
# Without a com.apple.access_ssh group, Remote Login means ALL users, and macOS's sshd lets every
# account in with its password. To keep every other account exactly as it was (no ssh), the
# group is created BEFORE Remote Login goes on, with only the AI account in it.
ensure_ssh_group() {
  if ! group_exists; then
    dseditgroup -o create -q "$ssh_group" || return 1
    made_group=1
  fi
  if ! dseditgroup -o checkmember -m "$user" "$ssh_group" >/dev/null 2>&1; then
    dseditgroup -o edit -a "$user" -t user "$ssh_group" >/dev/null 2>&1 \
      || dseditgroup -o checkmember -m "$user" "$ssh_group" >/dev/null 2>&1 || return 1
  fi
}
settings_steps() {
  cat <<EOS
  Open System Settings -> General -> Sharing and turn Remote Login on.
  Leave "Allow access for" alone: the installer puts '$fullname' on that list itself.
EOS
}
all_users_warning() {
  cat <<EOS
WARNING: Remote Login was already on, for ALL users. Every account on this Mac can log in over
ssh with its password (macOS's default), not only $user. The AI host kit changed nothing about
that. To restrict it: System Settings -> General -> Sharing -> Remote Login -> "Allow access
for" -> "Only these users", then add the people who need it and '$fullname'.
EOS
}
allow_list_warning() {
  cat <<EOS
WARNING: after Remote Login was switched on, the allow-list ($ssh_group) was missing or did not
list '$fullname', so ssh may be open to ALL users of this Mac (password login included) until it
is fixed. Check System Settings -> General -> Sharing -> Remote Login -> "Allow access for":
choose "Only these users" and list only '$fullname' (and whoever else should have ssh).
EOS
}
existing_group_note() {
  cat <<EOS
NOTE: the Remote Login allow-list ($ssh_group) already existed, so whoever it lists can log in
over ssh with a password (on a stock Mac it includes the Administrators group). The AI host kit
only adds $user to it and removes no one. To narrow it: System Settings -> General -> Sharing ->
Remote Login -> "Allow access for".
EOS
}
# Who else the allow-list names, one "user NAME", "group NAME (members: ...)" or "unknown GUID" per
# line (the AI account itself left out). Read-only.
guid_name() { # $1 GUID -> "group NAME" | "user NAME" | nothing
  _n=$(dscl . -search /Groups GeneratedUID "$1" 2>/dev/null | awk 'NR == 1 { print $1 }')
  if [ -n "$_n" ]; then printf 'group %s\n' "$_n"; return; fi
  _n=$(dscl . -search /Users GeneratedUID "$1" 2>/dev/null | awk 'NR == 1 { print $1 }')
  if [ -n "$_n" ]; then printf 'user %s\n' "$_n"; fi
}
group_others() {
  dseditgroup -o read "$ssh_group" 2>/dev/null | awk '
    /^dsAttrTypeStandard:/ { attr = $1; sub(/^dsAttrTypeStandard:/, "", attr); next }
    /^[ \t]/ { v = $0; gsub(/^[ \t]+/, "", v)
      if (attr == "GroupMembership") print "user " v
      else if (attr == "NestedGroups" || attr == "GroupMembers") print "guid " v }' |
  while read -r _k _v; do
    case $_k in
      user) [ "$_v" = "$user" ] || printf 'user %s\n' "$_v" ;;
      guid) _r=$(guid_name "$_v")
            case $_r in
              '') printf 'unknown %s\n' "$_v" ;;
              "user $user") ;;
              "group "*) _gm=$(dscl . -read "/Groups/${_r#group }" GroupMembership 2>/dev/null | sed 's/^GroupMembership: *//')
                         if [ -n "$_gm" ]; then printf '%s (members: %s)\n' "$_r" "$_gm"; else printf '%s\n' "$_r"; fi ;;
              *) printf '%s\n' "$_r" ;;
            esac ;;
    esac
  done |
  # dseditgroup lists a user member twice (by name, and by GUID under GroupMembers)
  awk '!seen[$0]++'
}
# Is TYPE NAME still named by the allow-list? Decided from the group record itself (what
# group_others parses), never from `dseditgroup -o checkmember`: that takes a user name only
# ("Unable to find the user record" for `-m admin -t group`).
still_listed() { # $1 user|group, $2 name
  group_others | awk -v t="$1" -v n="$2" '$1 == t && $2 == n { f = 1 } END { exit !f }'
}
# Take every entry of the resolved list ($1) out of the allow-list. Sets removed_members (for the
# cleanup) and _gone (for the message). The post-condition makes any mismatch with the tool fail
# loudly: Remote Login must never be turned on after a "narrowing" that did not narrow anything.
remove_others() {
  _gone=
  while read -r _k _v; do
    case $_k in
      user) _t=user; _nm=$_v ;;
      group) _t=group; _nm=${_v%% *} ;;
      *) die "cannot remove '$_k $_v' from $ssh_group by name; use System Settings -> General -> Sharing -> Remote Login -> Allow access for, or --ssh-keep-list." ;;
    esac
    if dseditgroup -o edit -d "$_nm" -t "$_t" "$ssh_group" >/dev/null 2>&1; then
      removed_members="$removed_members$_t:$_nm
"
      _gone="$_gone    - $_t $_nm
"
    elif still_listed "$_t" "$_nm"; then
      die "cannot remove $_t $_nm from $ssh_group. Nothing else was changed; Remote Login stays off."
    fi   # else: it is gone already (removed meanwhile, or listed twice): nothing to do
  done <<EOL
$1
EOL
  _left=$(group_others)
  [ -z "$_left" ] || die "could not narrow the allow-list $ssh_group: it still names
$(others_text "$_left")
Remote Login stays off; nothing else was changed. Use System Settings -> General -> Sharing -> Remote Login -> Allow access for, or --ssh-keep-list."
}
restore_removed() {
  while IFS=: read -r _t _n; do
    if [ -n "$_n" ]; then dseditgroup -o edit -a "$_n" -t "$_t" "$ssh_group" >/dev/null 2>&1 || :; fi
  done <<EOR
$removed_members
EOR
}
others_text() { printf '%s\n' "$1" | sed 's/^/    - /'; }
others_facts() { # $1 the list
  cat <<EOS
Remote Login is OFF now, and its allow-list ($ssh_group) also names:
$(others_text "$1")
Turning Remote Login on would give each of them password ssh from your network, where nobody has ssh today.
EOS
}
ssh_flags_help() {
  cat <<EOS
  --ssh-only-ai    remove them from the list first: only '$fullname' gets ssh (recommended)
  --ssh-keep-list  keep the list as it is, and turn Remote Login on
EOS
}
remove_hint() { # how to give someone ssh again
  echo "To let a user or group back in: System Settings -> General -> Sharing -> Remote Login -> Allow access for -> +, or: sudo dseditgroup -o edit -a NAME -t user|group $ssh_group"
}
rl_was_on=0; if remote_login_on; then rl_was_on=1; fi
grp=0; if group_exists; then grp=1; fi
others=""; if [ "$grp" = 1 ]; then others=$(group_others); fi
only_ai=0; if [ "$grp" = 0 ] || [ -z "$others" ]; then only_ai=1; fi
post_check_failed=0
stops=0
if [ "$dry" = 1 ]; then
  if [ "$rl_was_on" = 1 ] && [ "$grp" = 0 ]; then
    plan "Remote Login is already ON for all users (no $ssh_group group): left as it is, and the installer warns:"
    all_users_warning | sed 's/^/        | /'
  elif [ "$rl_was_on" = 1 ]; then
    plan "Remote Login is already on, limited to the users in $ssh_group: add $user to it (dseditgroup -o edit -a $user -t user $ssh_group)"
    existing_group_note | sed 's/^/        | /'
  else
    if [ "$grp" = 0 ]; then
      plan "Remote Login is off and there is no $ssh_group group. Turning it on would let EVERY account in over ssh, so first:"
      plan "  dseditgroup -o create -q $ssh_group   then   dseditgroup -o edit -a $user -t user $ssh_group"
      plan "  (only $user is in it: every other account keeps 'no ssh' as before; the group is removed again only if the run fails before Remote Login is attempted)"
    elif [ -z "$others" ]; then
      plan "Remote Login is off; the group $ssh_group lists nobody but the AI account: add $user to it if needed"
    else
      plan "Remote Login is off and the group $ssh_group also names (resolved):"
      others_text "$others" | sed 's/^/        | /'
      plan "  turning it on would give each of them password ssh from the network. So the installer does NOT turn it on without a choice:"
      case $ssh_choice in
        only-ai) plan "  --ssh-only-ai given: it removes them from the list first (restored if the run fails before Remote Login is attempted), then turns Remote Login on" ;;
        keep)    plan "  --ssh-keep-list given: it keeps the list and turns Remote Login on" ;;
        *) if [ -t 0 ]; then plan "  on this terminal it would ask: (a) only $fullname (default, Enter), (b) keep the list, (c) stop"
           else plan "  without a terminal it would STOP before systemsetup and print the facts and the flags:"; ssh_flags_help | sed 's/^/        | /'; stops=1; fi ;;
      esac
    fi
    if [ "$stops" = 1 ]; then
      plan "(a run like that stops here, exit 1, Remote Login still off; the steps below happen once you answer on a terminal or pass a flag)"
    else
      plan "turn Remote Login on: systemsetup -setremotelogin on"
      plan "if that fails (the terminal has no Full Disk Access) print this, and wait for Enter until Remote Login is on:"
      settings_steps | sed 's/^/        | /'
      plan "  (the allow-list is kept in that case; after the switch-on it is checked, and the summary warns if it is not what it should be)"
    fi
  fi
else
  if [ "$rl_was_on" = 0 ]; then
    if [ "$grp" = 1 ] && [ -n "$others" ]; then
      others_facts "$others" >&2
      choice=$ssh_choice
      if [ -z "$choice" ]; then
        if [ -t 0 ]; then
          tries=0
          while [ -z "$choice" ]; do
            tries=$((tries + 1))
            [ "$tries" -le 3 ] || die "no valid answer. Nothing was changed."
            printf '  (a) only %s may use ssh: remove the others from the list (recommended)\n  (b) keep the list as it is and turn Remote Login on\n  (c) stop\nChoose [a]: ' "$fullname" >&2
            read -r _ans || die "no answer. Nothing was changed."
            case ${_ans:-a} in a|A) choice=only-ai ;; b|B) choice=keep ;; c|C) die "stopped at your request. Nothing was changed." ;; esac
          done
        else
          warn "There is no terminal to ask on, so Remote Login stays off. Nothing was changed about ssh. Run again with one of:"
          ssh_flags_help >&2
          exit 1
        fi
      fi
      if [ "$choice" = only-ai ]; then
        remove_others "$others"
        say "removed from the Remote Login allow-list ($ssh_group):"
        printf '%s' "$_gone" >&2
        remove_hint >&2
        only_ai=1
      else
        say "keeping the allow-list as it is (--ssh-keep-list / your choice)"
      fi
    fi
    say "limiting ssh to $user before turning Remote Login on ($ssh_group)"
    ensure_ssh_group || die "cannot set up $ssh_group for $user (dseditgroup, message above). Nothing was changed about Remote Login."
    say "turning Remote Login on"
    rl_attempted=1
    systemsetup -setremotelogin on >"$work/sysetup.out" 2>&1 || :
    # systemsetup can print its Full Disk Access complaint and still exit 0, so ask the system.
    sleep 1
    tries=0
    while ! remote_login_on; do
      cat "$work/sysetup.out" 2>/dev/null || :
      echo >&2
      warn "macOS did not let this terminal turn Remote Login on (it needs Full Disk Access)."
      settings_steps >&2
      warn "The allow-list ($ssh_group) stays as the installer left it, so switching Remote Login on later does not open ssh to everyone."
      tries=$((tries + 1))
      [ -t 0 ] || die "no terminal to wait on. Do the steps above, then run this installer again."
      [ "$tries" -le 3 ] || die "Remote Login is still off. Do the steps above, then run this installer again."
      printf 'Then press Enter. ' >&2
      read -r _ || die "no answer; run this installer again after Remote Login is on."
      : > "$work/sysetup.out"
      sleep 1
    done
    say "Remote Login is on"
    # macOS may rewrite the allow-list when Remote Login is switched on: check it is still ours.
    if ! group_exists; then post_check_failed=1; fi
    if ensure_ssh_group; then
      if [ "$post_check_failed" = 1 ]; then post_check_failed=2; fi   # recreated: ssh may have been open to all for a moment
    else
      post_check_failed=1
    fi
    if [ "$post_check_failed" != 0 ]; then
      warn "$ssh_group was missing or lost $user when Remote Login was switched on, so ssh may have been open to ALL users. See the summary."
    else
      others=$(group_others)   # the list as it stands now
      if [ -z "$others" ]; then only_ai=1; else only_ai=0; fi
    fi
    if [ "$only_ai" = 0 ]; then existing_group_note >&2; fi
  elif [ "$grp" = 1 ]; then
    say "Remote Login is already on, limited to the users in $ssh_group"
    ensure_ssh_group || die "cannot add $user to $ssh_group. In System Settings -> General -> Sharing -> Remote Login, add '$fullname' under 'Only these users'."
    say "$user is allowed over ssh ($ssh_group)"
    if [ "$only_ai" = 0 ]; then existing_group_note >&2; fi
  else
    say "Remote Login is already on, for all users"
    all_users_warning >&2
  fi
fi

# ---------------------------------------------------------------- 5. CLIs ---------------
cli_ok=1
if [ "$dry" = 1 ]; then
  if [ "${JODD_AI_SKIP_CLIS:-}" = 1 ]; then plan "skip the CLI install (JODD_AI_SKIP_CLIS=1)"
  else
    plan "install claude and codex as $user, each through its login shell (zsh -lc), from the official installers:"
    plan "  curl -fsSL https://claude.ai/install.sh | bash"
    plan "  curl -fsSL https://chatgpt.com/codex/install.sh | sh"
  fi
elif [ "${JODD_AI_SKIP_CLIS:-}" = 1 ]; then
  say "skipping the CLI install (JODD_AI_SKIP_CLIS=1). Later: sudo jodd-ai-host update-clis"
else
  say "installing claude and codex for $user (official installers; this can take a few minutes)"
  if ! "$helper" update-clis; then
    cli_ok=0
    warn "At least one CLI did not install (no network?). Everything else is set up. Retry later: sudo jodd-ai-host update-clis"
  fi
fi

# ---------------------------------------------------------------- sleep -----------------
if [ "$nosleep" = 1 ]; then
  if [ "$dry" = 1 ]; then plan "keep the Mac awake on its charger: pmset -c sleep 0 disksleep 0"
  elif pmset -c sleep 0 disksleep 0; then say "the Mac no longer sleeps on its charger (pmset -c sleep 0 disksleep 0)"
  else warn "pmset failed; set it in System Settings (below)."; fi
fi

# ---------------------------------------------------------------- 6. summary ------------
if [ "$dry" = 1 ]; then
  plan "then: jodd-ai-host doctor, and print the host, port, account and host-key fingerprint"
  homes=$(open_homes)
  if [ -n "$homes" ]; then
    plan "and list the home folders other accounts can enter, with advice only (nothing is changed):"
    open_homes_text "$homes" | sed 's/^/        | /'
  fi
  echo
  say "DRY RUN finished. Nothing was changed on this Mac."
  exit 0
fi
echo
"$helper" doctor || :
echo
fp=$("$helper" fingerprint 2>/dev/null || echo "(unreadable - run: sudo jodd-ai-host doctor)")
addrs=$(ifconfig 2>/dev/null | awk '$1 == "inet" && $2 !~ /^127\./ && $2 !~ /^169\.254\./ { printf "%s ", $2 }' || :)
lan=$(scutil --get LocalHostName 2>/dev/null || :)
[ -z "$lan" ] || addrs="$addrs$lan.local"
addrs=$(printf '%s' "$addrs" | sed 's/[[:space:]]*$//')
[ -n "$addrs" ] || addrs="(this Mac's address)"
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

Keep this Mac awake, or Jodd cannot reach it while you are away:
  System Settings -> Energy -> "Prevent automatic sleeping when the display is off"
  (on a laptop: Battery -> Options), or run this installer again with --no-sleep.
EOS
if [ "$post_check_failed" != 0 ]; then
  echo
  allow_list_warning
elif [ "$rl_was_on" = 1 ] && [ "$grp" = 0 ]; then
  echo
  all_users_warning
elif [ "$only_ai" = 1 ]; then
  echo
  echo "ssh is limited to $user ($ssh_group): no other account on this Mac can log in over ssh."
  if [ -n "$removed_members" ]; then
    echo "Removed from the allow-list on your say-so:"
    printf '%s' "$removed_members" | sed 's/^\([a-z]*\):\(.*\)$/    - \1 \2/'
    remove_hint
  else
    echo "To let others in, add them in System Settings -> General -> Sharing -> Remote Login."
  fi
else
  echo
  existing_group_note
fi
homes=$(open_homes)
if [ -n "$homes" ]; then
  echo
  open_homes_text "$homes"
fi
if [ "$locked" = yes ]; then
  echo
  echo "Password login for $user is LOCKED (kept from before). Before Setup in Jodd: sudo jodd-ai-host unlock"
fi
if [ "$cli_ok" = 0 ]; then
  echo
  echo "claude and/or codex are not installed yet: sudo jodd-ai-host update-clis"
fi
echo
echo "More: $manual_url#macos"
