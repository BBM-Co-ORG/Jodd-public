# Shared by the e2e drivers. Source it; do not run it.
repo=$(cd "$(dirname "$0")/../.." && pwd)

e2e_run_jodd_tests() { # $1 port, $2 container, $3 password
  # An unaccepted Xcode licence breaks the linker on a Mac; the command-line tools still link.
  if [ "$(uname)" = Darwin ] && [ -z "${DEVELOPER_DIR:-}" ] && [ -d /Library/Developer/CommandLineTools ]; then
    export DEVELOPER_DIR=/Library/Developer/CommandLineTools
  fi
  ( cd "$repo/src-tauri" && JODD_AI_E2E="127.0.0.1:$1" JODD_AI_E2E_CONTAINER="$2" JODD_AI_E2E_PASSWORD="$3" \
    cargo test -p jodd --lib llm::remote::host_kit_tests -- --ignored --nocapture --test-threads=1 )
}

e2e_wait_port() { # $1 port — up to 60 s
  i=0
  while ! nc -z 127.0.0.1 "$1" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -gt 60 ] && return 1
    sleep 1
  done
}

# A real, long-lived ssh session (password login through askpass) that runs `cat`, so the test
# can tell whether it is still alive later: it is, if a word sent now comes back.
# Used to prove that reloading sshd (lock / unlock / the installer's re-run) does not kill
# open sessions - before OpenSSH 9.8 a `pkill -HUP -x sshd` does.
e2e_session_start() { # $1 port, $2 user, $3 password
  E2E_SD=$(mktemp -d)
  mkfifo "$E2E_SD/in"; : > "$E2E_SD/out"
  # shellcheck disable=SC2016  # $E2E_PW is for the askpass script
  printf '#!/bin/sh\nprintf "%%s\\n" "$E2E_PW"\n' > "$E2E_SD/ask"; chmod 700 "$E2E_SD/ask"
  ( E2E_PW=$3 SSH_ASKPASS="$E2E_SD/ask" SSH_ASKPASS_REQUIRE=force DISPLAY=none exec ssh \
      -o PubkeyAuthentication=no -o PreferredAuthentications=password,keyboard-interactive \
      -o NumberOfPasswordPrompts=1 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
      -o LogLevel=ERROR -o ConnectTimeout=10 -p "$1" "$2@127.0.0.1" cat \
      < "$E2E_SD/in" > "$E2E_SD/out" 2>/dev/null ) &
  E2E_SPID=$!
  exec 3> "$E2E_SD/in"
  e2e_session_ping hello
}
e2e_session_ping() { # $1 word: send it, wait up to 20 s for it to come back
  # in a subshell: writing to a dead session raises SIGPIPE, which must not end the whole driver
  ( printf '%s\n' "$1" >&3 ) 2>/dev/null || return 1
  _i=0
  until grep -qx "$1" "$E2E_SD/out" 2>/dev/null; do
    _i=$((_i + 1)); [ "$_i" -gt 40 ] && return 1
    kill -0 "$E2E_SPID" 2>/dev/null || return 1
    sleep 0.5
  done
}
e2e_session_end() {
  exec 3>&-
  wait "$E2E_SPID" 2>/dev/null || :
  rm -rf "$E2E_SD"
}
