# Renders Jodd's sshd drop-in, and judges the sshd settings the drop-in does not set. Sourced
# by jodd-ai-host, the installers and the image's entrypoint, so all of them agree.
# Usage: render_dropin USER LOCKED   (LOCKED = yes | no)
render_dropin() {
  _u=$1 _locked=$2
  if [ "$_locked" = yes ]; then _pw=no; else _pw=yes; fi
  cat <<EOF2
# Managed by jodd-ai-host. Change with: jodd-ai-host lock | unlock
# Only the account below is affected; every other account keeps the server's settings.
Match User $_u
    PasswordAuthentication $_pw
    KbdInteractiveAuthentication no
    ClientAliveInterval 30
    ClientAliveCountMax 4
EOF2
}

# The sshd settings, besides PasswordAuthentication, that can still keep the AI account out:
# AuthenticationMethods (a global `publickey` refuses a password even where the drop-in allows
# one) and AllowUsers / AllowGroups / DenyUsers / DenyGroups. They are the admin's: the kit never
# overrides them, it only says which one to edit. Reads `sshd -T -C user=USER,...` on stdin and
# prints one line per setting that keeps USER out - or that cannot be judged from here (a
# user@host entry) - naming the setting. Prints nothing when none does.
# Used by the installers (after the drop-in is in place) and by `jodd-ai-host doctor`.
# Usage: sshd_login_problems USER "GROUP ..." < sshd-T-output
sshd_login_problems() {
  _lu=$1 _lg=$2 _au=0 _auok=0 _auhost='' _ag=0 _agok=0
  while read -r _k _v; do
    case $_k in
      authenticationmethods)
        # Jodd's Setup signs in with a password and nothing else (no key exists yet), so one of
        # the space-separated lists must be `password` on its own; `any` is sshd's default.
        _pwok=0
        for _l in $_v; do case $_l in any|password|password:*) _pwok=1 ;; esac; done
        [ "$_pwok" = 1 ] || echo "AuthenticationMethods is '$_v' for $_lu, so sshd refuses a password on its own and Jodd's Setup cannot sign in. Edit AuthenticationMethods in sshd_config so $_lu may use 'password' alone (e.g. a 'Match User $_lu' block of your own with 'AuthenticationMethods password')." ;;
      denyusers)
        if _sshd_pat_match "$_lu" "${_v%%@*}"; then
          case $_v in
            *@*) echo "DenyUsers has '$_v': sshd refuses $_lu from the hosts it names, and whether that includes your devices cannot be told from here. Check DenyUsers in sshd_config." ;;
            *)   echo "DenyUsers names $_lu ('$_v'), so sshd refuses it. Take it out of DenyUsers in sshd_config." ;;
          esac
        fi ;;
      allowusers)
        _au=1
        if _sshd_pat_match "$_lu" "${_v%%@*}"; then
          case $_v in *@*) _auhost="$_auhost $_v" ;; *) _auok=1 ;; esac
        fi ;;
      denygroups)
        for _g in $_lg; do
          if _sshd_pat_match "$_g" "$_v"; then echo "DenyGroups names $_g ('$_v'), a group of $_lu, so sshd refuses it. Edit DenyGroups in sshd_config."; break; fi
        done ;;
      allowgroups)
        _ag=1
        for _g in $_lg; do if _sshd_pat_match "$_g" "$_v"; then _agok=1; break; fi; done ;;
    esac
  done
  if [ "$_au" = 1 ] && [ "$_auok" = 0 ]; then
    if [ -n "$_auhost" ]; then
      echo "AllowUsers lets $_lu in only from some hosts ($_auhost ); whether your devices are among them cannot be told from here. Check AllowUsers in sshd_config."
    else
      echo "AllowUsers does not name $_lu, so sshd refuses it. Add $_lu to AllowUsers in sshd_config."
    fi
  fi
  if [ "$_ag" = 1 ] && [ "$_agok" = 0 ]; then
    echo "AllowGroups names none of $_lu's groups (${_lg:-none found}), so sshd refuses it. Add one of them to AllowGroups in sshd_config."
  fi
}
# NAME PATTERN: does NAME match an sshd user/group pattern? sshd knows * and ?, which mean the
# same in a shell `case`; a pattern with [ or \ is compared literally (sshd has no brackets).
_sshd_pat_match() {
  case $2 in *\[*|*\\*) [ "$1" = "$2" ]; return ;; esac
  # shellcheck disable=SC2254  # the pattern is meant to match
  case $1 in $2) return 0 ;; esac
  return 1
}
