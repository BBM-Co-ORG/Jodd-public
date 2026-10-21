#!/bin/sh
# shellcheck disable=SC2012  # ls only reads folders this test created, with plain names
# shellcheck disable=SC2016  # fake scripts hold a literal $ for the shell that runs them
# Tests for the allow-list helpers of ai-host/install/macos.sh against fake dseditgroup / dscl
# (nothing touches this Mac). Run: sh ai-host/test/macos_test.sh
set -u
here=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
pass=0 fail=0
ok()   { pass=$((pass+1)); }
bad()  { fail=$((fail+1)); echo "FAIL: $1"; }
check(){ if [ "$2" = "$3" ]; then ok; else bad "$1: expected [$3] got [$2]"; fi; }

mkdir -p "$t/bin"
# The list: users root and kaiwanh (each shown twice by `read`: by name and by GUID), the AI
# account, and the admin group nested. The state file holds "type name" lines.
printf 'user root\nuser kaiwanh\nuser jodd-ai\ngroup admin\n' > "$t/members"
cat > "$t/bin/dseditgroup" <<FAKE
#!/bin/sh
m=$t/members
has() { grep -qx "\$1 \$2" "\$m"; }
case "\$2" in
  read)
    echo "dsAttrTypeStandard:GroupMembership -"
    awk '\$1 == "user" { print "\\t\\t" \$2 }' "\$m"
    echo "dsAttrTypeStandard:GroupMembers -"
    awk '\$1 == "user" { print "\\t\\tGUID-" \$2 }' "\$m"
    echo "dsAttrTypeStandard:NestedGroups -"
    awk '\$1 == "group" { print "\\t\\tGUID-" \$2 }' "\$m"
    echo "dsAttrTypeStandard:RecordName -"; echo "\\t\\tcom.apple.access_ssh" ;;
  checkmember)  # -o checkmember -m NAME [-t TYPE] GROUP: like the real tool, NAME is a USER name
    if [ "\$5" = -t ] && [ "\$6" != user ]; then echo "Unable to find the user record" >&2; exit 64; fi
    has user "\$4" ;;
  edit)
    op=\$3; name=\$4; type=\$6
    if [ "\$op" = -d ]; then
      has "\$type" "\$name" || { echo "not a member" >&2; exit 1; }
      [ -z "\${STUCK:-}" ] || exit 1
      [ -z "\${SILENT:-}" ] || exit 0   # claims success, changes nothing
      grep -vx "\$type \$name" "\$m" > "\$m.new"; mv "\$m.new" "\$m"
    else
      has "\$type" "\$name" || echo "\$type \$name" >> "\$m"
    fi ;;
esac
FAKE
cat > "$t/bin/dscl" <<FAKE
#!/bin/sh
# dscl . -search /Groups|/Users GeneratedUID GUID ;  dscl . -read /Groups/NAME GroupMembership
case "\$2" in
  -search) n=\${5#GUID-}
    if [ "\$3" = /Groups ]; then grep -qx "group \$n" $t/members && printf '%s\\t\\tGeneratedUID = (\\n    "%s"\\n)\\n' "\$n" "\$5"
    else grep -qx "user \$n" $t/members && printf '%s\\t\\tGeneratedUID = (\\n    "%s"\\n)\\n' "\$n" "\$5"; fi ;;
  -read) echo "GroupMembership: root kaiwanh" ;;
esac
exit 0
FAKE
chmod +x "$t/bin/dseditgroup" "$t/bin/dscl"
PATH="$t/bin:$PATH"

# Pull the helpers out of the installer (they are plain functions).
fn() { sed -n "/^$1() {/,/^}/p" "$here/install/macos.sh"; }
{ echo 'ssh_group=com.apple.access_ssh; user=jodd-ai; removed_members=""; die() { echo "DIE: $*"; exit 1; }'
  for f in guid_name group_others still_listed others_text remove_others restore_removed; do fn "$f"; done; } > "$t/lib.sh"
for f in guid_name group_others still_listed others_text remove_others restore_removed; do
  if [ -n "$(fn "$f")" ]; then ok; else bad "$f not found in macos.sh"; fi
done

# 1. each entry once, the AI account left out
got=$(sh -c '. "$1"; group_others' sh "$t/lib.sh")
want='user root
user kaiwanh
group admin (members: root kaiwanh)'
check "group_others lists every entry exactly once" "$got" "$want"

# 2. option (a) over that list: every user removed once, the group too, then idempotent
out=$(sh -c '. "$1"; others=$(group_others); remove_others "$others"; printf "%s" "$removed_members"; echo "--"; printf "%s" "$_gone"' sh "$t/lib.sh" 2>&1)
check "remove_others removes each entry once" "$out" 'user:root
user:kaiwanh
group:admin
--
    - user root
    - user kaiwanh
    - group admin'
check "only the AI account is left" "$(cat "$t/members")" "user jodd-ai"

# 3. removing what is already gone is not an error (and not recorded)
out=$(sh -c '. "$1"; remove_others "user root
user kaiwanh"; echo "rc=$?"; printf "[%s]" "$removed_members"' sh "$t/lib.sh" 2>&1)
check "a non-member counts as removed" "$out" 'rc=0
[]'

# 4. a member that cannot be removed is an error, and so is a removal that silently does nothing
printf 'user root\nuser jodd-ai\n' > "$t/members"
out=$(STUCK=1 sh -c '. "$1"; remove_others "user root"' sh "$t/lib.sh" 2>&1)
check "remove_others dies on a stuck member" "$out" "DIE: cannot remove user root from com.apple.access_ssh. Nothing else was changed; Remote Login stays off."
printf 'user root\ngroup admin\nuser jodd-ai\n' > "$t/members"
out=$(SILENT=1 sh -c '. "$1"; remove_others "user root
group admin"; echo "not reached"' sh "$t/lib.sh" 2>&1)
case $out in
  *"DIE: could not narrow the allow-list com.apple.access_ssh: it still names"*"user root"*"group admin"*) ok ;;
  *) bad "the post-condition did not fire when removals silently do nothing: [$out]" ;;
esac
check "the silent no-op left the list as it was" "$(sort "$t/members" | tr '\n' ',')" "group admin,user jodd-ai,user root,"

# 4b. the real tool cannot checkmember a group: the fake says so, and nothing relies on it
printf 'user jodd-ai\ngroup admin\n' > "$t/members"
dseditgroup -o checkmember -m admin -t group com.apple.access_ssh >/dev/null 2>&1; check "fake: checkmember -t group fails like the real tool (rc 64)" "$?" "64"
out=$(sh -c '. "$1"; remove_others "$(group_others)"; echo "rc=$?"; printf "%s" "$removed_members"' sh "$t/lib.sh" 2>&1)
check "a nested group is removed although checkmember cannot see it" "$out" 'rc=0
group:admin'
check "the nested group is gone from the record" "$(cat "$t/members")" "user jodd-ai"

# 5. restore puts back what was taken out
printf 'user jodd-ai\n' > "$t/members"
sh -c '. "$1"; removed_members="user:root
group:admin
"; restore_removed' sh "$t/lib.sh"
check "restore_removed re-adds members and groups" "$(sort "$t/members" | tr '\n' ',')" "group admin,user jodd-ai,user root,"

# ---- the AI account's own group, closed home, staff post-check, and the summary's home list ----
# Fakes for dscl / dseditgroup / dsmemberutil keep their state in $DS ("<path>.<attr>" files);
# every change they make is logged to $DS/changes, so a dry run can be shown to change nothing.
DS=$t/ds; export DS
mkdir -p "$t/bin2"
cat > "$t/bin2/dscl" <<'FAKE'
#!/bin/sh
# dscl . -read /Users|/Groups/NAME ATTR ;  dscl . -create /Users/NAME ATTR VALUE
f="$DS$3.$4"
case $2 in
  -read) [ -f "$f" ] || { echo "No such key: $4" >&2; exit 181; }; echo "$4: $(cat "$f")" ;;
  -create) echo "dscl $*" >> "$DS/changes"; mkdir -p "${f%/*}"; echo "$5" > "$f" ;;
  *) exit 1 ;;
esac
FAKE
cat > "$t/bin2/dseditgroup" <<'FAKE'
#!/bin/sh
# dseditgroup -o create -r FULLNAME GROUP: the new group gets gid 701
[ "$2" = create ] || exit 1
echo "dseditgroup $*" >> "$DS/changes"
mkdir -p "$DS/Groups"; echo 701 > "$DS/Groups/$5.PrimaryGroupID"
FAKE
cat > "$t/bin2/dsmemberutil" <<'FAKE'
#!/bin/sh
# dsmemberutil flushcache | checkmembership -U USER -G staff: a member while its primary gid is 20
# (or always, with FAKE_STAFF_STUCK=1: macOS keeping it in staff some other way)
case $1 in
  flushcache) exit 0 ;;
  checkmembership)
    if [ -n "${FAKE_STAFF_STUCK:-}" ] || [ "$(cat "$DS/Users/$3.PrimaryGroupID" 2>/dev/null)" = 20 ]; then
      echo "user is a member of the group"
    else echo "user is not a member of the group"; fi ;;
esac
FAKE
chmod +x "$t/bin2/dscl" "$t/bin2/dseditgroup" "$t/bin2/dsmemberutil"
{ echo 'user=jodd-ai; fullname="Jodd AI host"; staff_gid=20; users_dir=$T/Users; home=$T/Users/jodd-ai; dry=${DRY:-0}'
  echo 'say() { echo "SAY: $*"; }; warn() { echo "WARN: $*"; }; die() { echo "DIE: $*"; exit 1; }'
  grep -E '^(plan|dsread|sq)\(\) \{.*\}$' "$here/install/macos.sh"
  echo 'in_staff=unknown'
  for f in own_group close_home staff_check open_homes open_homes_text; do fn "$f"; done; } > "$t/lib2.sh"
for f in plan dsread sq; do if grep -q "^$f() {" "$t/lib2.sh"; then ok; else bad "$f not found in macos.sh"; fi; done
for f in own_group close_home staff_check open_homes open_homes_text; do
  if [ -n "$(fn "$f")" ]; then ok; else bad "$f not found in macos.sh"; fi
done
# acct GID [GROUPGID]: a fresh state - the account's primary gid, optionally a group named like it;
# its home 750 (as sysadminctl makes it). GID "" = no account yet (a dry run before creating it).
acct() {
  rm -rf "$DS" "$t/Users"; mkdir -p "$DS/Users" "$DS/Groups" "$t/Users/jodd-ai"; chmod 750 "$t/Users/jodd-ai"
  [ -z "$1" ] || echo "$1" > "$DS/Users/jodd-ai.PrimaryGroupID"
  [ -z "${2:-}" ] || echo "$2" > "$DS/Groups/jodd-ai.PrimaryGroupID"
}
run2() { T=$t PATH="$t/bin2:$PATH" sh -c '. "$1"; own_group; close_home; staff_check; echo "in_staff=$in_staff"' sh "$t/lib2.sh" 2>&1; }
modeof() { ls -ld "$1" | cut -c1-10; }

# 6. a new account (sysadminctl gave it staff, 20): its own group, primary; home 700; out of staff
acct 20
out=$(run2)
check "new account: the group is created, then made primary" "$(cat "$DS/changes")" 'dseditgroup -o create -r Jodd AI host jodd-ai
dscl . -create /Users/jodd-ai PrimaryGroupID 701'
check "new account: primary gid is its own" "$(cat "$DS/Users/jodd-ai.PrimaryGroupID")" "701"
check "new account: home is 700" "$(modeof "$t/Users/jodd-ai")" "drwx------"
case $out in *"in_staff=0"*) ok ;; *) bad "new account: still in staff: [$out]" ;; esac
case $out in *WARN*|*DIE*) bad "new account: unexpected warning: [$out]" ;; *) ok ;; esac

# 7. an existing account still on staff (installed before this fix), its group already there: migrated
acct 20 702
out=$(run2)
check "existing account on gid 20: only the primary group changes" "$(cat "$DS/changes")" 'dscl . -create /Users/jodd-ai PrimaryGroupID 702'
check "existing account: home is 700" "$(modeof "$t/Users/jodd-ai")" "drwx------"
case $out in *"in_staff=0"*) ok ;; *) bad "existing account: still in staff: [$out]" ;; esac
# ... and one with a group of its own already is left alone
acct 702 702
out=$(run2)
check "an account with its own group: no change" "$(cat "$DS/changes" 2>/dev/null)" ""
case $out in *"primary group is 702, not staff: left as it is"*) ok ;; *) bad "no 'left as it is': [$out]" ;; esac
# ... a group named like the account but with a system id is refused, not adopted
acct 20 80
out=$(run2)
case $out in *"DIE: the group jodd-ai has the id 80"*) ok ;; *) bad "a system-id group was adopted: [$out]" ;; esac
check "a refused group changes nothing" "$(cat "$DS/changes" 2>/dev/null)" ""

# 8. dry run: prints the plan, changes nothing (no account yet, and an existing one on staff)
for st in "" 20; do
  acct "$st"
  out=$(DRY=1 run2)
  check "dry run ($st): nothing changed in the directory" "$(cat "$DS/changes" 2>/dev/null)" ""
  check "dry run ($st): home mode unchanged" "$(modeof "$t/Users/jodd-ai")" "drwxr-x---"
  case $out in *'[dry-run] give jodd-ai a group of its own: dseditgroup -o create -r "Jodd AI host" jodd-ai'*) ok ;; *) bad "dry run ($st): no group plan: [$out]" ;; esac
  case $out in *"[dry-run] make it jodd-ai's primary group instead of staff (gid 20): dscl . -create /Users/jodd-ai PrimaryGroupID"*) ok ;; *) bad "dry run ($st): no primary-group plan: [$out]" ;; esac
  case $out in *"[dry-run] close $t/Users/jodd-ai to other accounts: chmod 700 $t/Users/jodd-ai"*) ok ;; *) bad "dry run ($st): no chmod plan: [$out]" ;; esac
  case $out in *"[dry-run] then check: dsmemberutil checkmembership -U jodd-ai -G staff"*) ok ;; *) bad "dry run ($st): no post-check plan: [$out]" ;; esac
  if printf '%s\n' "$out" | grep -Eq '^(SAY|WARN|DIE):'; then bad "dry run ($st): a non-plan line: [$out]"; else ok; fi
done

# 9. macOS keeps it in staff anyway: a clear warning, the run goes on
acct 20
out=$(FAKE_STAFF_STUCK=1 run2)
case $out in *"WARN: WARNING: jodd-ai is still a member of the staff group"*"in_staff=1"*) ok ;; *) bad "no warning when still in staff: [$out]" ;; esac
case $out in *DIE*) bad "the staff post-check died: [$out]" ;; *) ok ;; esac

# 10. root does not chmod through a symlinked home
acct 20; mkdir -p "$t/elsewhere"; chmod 755 "$t/elsewhere"; rm -rf "$t/Users/jodd-ai"; ln -s "$t/elsewhere" "$t/Users/jodd-ai"
out=$(run2)
check "a symlinked home's target is not chmodded" "$(modeof "$t/elsewhere")" "drwxr-xr-x"
case $out in *"WARN: $t/Users/jodd-ai is a symbolic link"*) ok ;; *) bad "no warning for a symlinked home: [$out]" ;; esac

# 11. the summary lists other homes staff or everyone can enter, with chmod advice; changes nothing
acct 702 702
mkdir -p "$t/Users/alice" "$t/Users/bob" "$t/Users/carol" "$t/Users/Shared" "$t/Users/Guest" "$t/Users/dave"
chmod 750 "$t/Users/alice"; chmod 755 "$t/Users/bob"; chmod 700 "$t/Users/carol"; chmod 1777 "$t/Users/Shared"; chmod 755 "$t/Users/Guest" "$t/Users/jodd-ai"
sg=$(ls -lnd "$t/Users/alice" | awk '{ print $4 }')   # the test's "staff": the group these folders got
# dave: group-enterable, but by a group other than "staff" (another group the tester is in)
og=$(id -G | tr ' ' '\n' | grep -vx "$sg" | head -n 1)
if [ -n "$og" ] && chgrp "$og" "$t/Users/dave" 2>/dev/null; then chmod 750 "$t/Users/dave"; else rmdir "$t/Users/dave"; fi
homes() { T=$t SG=$1 IS=$2 sh -c '. "$1"; staff_gid=$SG; in_staff=$IS; h=$(open_homes); printf "%s\n--\n" "$h"; open_homes_text "$h"' sh "$t/lib2.sh"; }
out=$(homes "$sg" 0)
check "open_homes: alice (staff) and bob (everyone), nobody else" "$(printf '%s\n' "$out" | sed '/^--$/q')" "$t/Users/alice staff
$t/Users/bob everyone
--"
case $out in *"chmod 700 '$t/Users/alice'"*"chmod 700 '$t/Users/bob'"*) ok ;; *) bad "no chmod advice for each: [$out]" ;; esac
case $out in *"chmod 700 '$t/Users/bob'     (open to every account, jodd-ai included"*) ok ;; *) bad "bob not marked as open to everyone: [$out]" ;; esac
case $out in *"jodd-ai has a group of its own"*) ok ;; *) bad "the text does not say the account left staff: [$out]" ;; esac
case $out in *carol*|*Shared*|*Guest*|*"chmod 700 $t/Users/jodd-ai"*) bad "listed a closed, special or the AI account's folder: [$out]" ;; *) ok ;; esac
# control: with "staff" = dave's group, dave is listed and alice is not
if [ -d "$t/Users/dave" ]; then
  case $(homes "$og" 0) in *"$t/Users/dave staff"*) case $(homes "$og" 0) in *"$t/Users/alice"*) bad "alice listed under another staff gid" ;; *) ok ;; esac ;; *) bad "dave not listed when its group is staff" ;; esac
fi
# a folder name with a space is listed and advised whole
mkdir "$t/Users/Ann Lee"; chmod 755 "$t/Users/Ann Lee"
case $(homes "$sg" 0) in *"chmod 700 '$t/Users/Ann Lee'     (open to every account"*) ok ;; *) bad "a home with a space in its name was not advised whole" ;; esac
rmdir "$t/Users/Ann Lee"
out=$(homes "$sg" 1)
case $out in *"jodd-ai is STILL in the staff group, so it can enter these home folders"*) ok ;; *) bad "post-check failed, but the text does not say so: [$out]" ;; esac
check "listing the homes changed no mode" "$(modeof "$t/Users/alice") $(modeof "$t/Users/bob")" "drwxr-x--- drwxr-xr-x"

echo "macos tests: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
