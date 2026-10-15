#!/bin/sh
set -eu
# shellcheck disable=SC1091
. /usr/local/lib/jodd-ai-host/dropin.sh
state=/var/lib/jodd-ai-host
key=$state/ssh_host_ed25519_key
[ -f "$key" ] || ssh-keygen -q -t ed25519 -N '' -f "$key"
# Always rebuild the .pub from the private key: a missing or damaged .pub must not break the fingerprint.
if ssh-keygen -y -f "$key" > "$key.pub.new" 2>/dev/null; then
  mv "$key.pub.new" "$key.pub"
else
  rm -f "$key.pub.new"
  echo "jodd-ai-host: the host key $key cannot be read, so sshd cannot start. Restore it from a backup, or remove it (and its .pub) to get a new key; Jodd will then show a new fingerprint to confirm." >&2
  exit 1
fi
cat > /etc/jodd-ai-host.conf <<EOC
JODD_AI_USER=jodd-ai
JODD_AI_PORT=${JODD_AI_PORT:-2222}
JODD_AI_KIND=docker
JODD_AI_DROPIN=$state/50-jodd-ai.conf
JODD_AI_HOSTKEY=$key.pub
JODD_AI_LIB=/usr/local/lib/jodd-ai-host
EOC
printf 'HostKey %s\n' "$key" > /etc/ssh/sshd_config.d/00-jodd-hostkey.conf
# Global defaults are closed; only the AI account's Match block (50-) opens password
# login, and only while it is unlocked. Root never logs in over ssh.
printf 'PermitRootLogin no\nPasswordAuthentication no\n' > /etc/ssh/sshd_config.d/01-jodd-hardening.conf
# Keep lock / unlock across restarts: the drop-in lives in the state volume and
# the helper edits it there (JODD_AI_DROPIN above); sshd reads it through this link.
[ -f "$state/50-jodd-ai.conf" ] || render_dropin jodd-ai no > "$state/50-jodd-ai.conf"
ln -sf "$state/50-jodd-ai.conf" /etc/ssh/sshd_config.d/50-jodd-ai.conf
chown jodd-ai:jodd-ai /home/jodd-ai
# The home volume can predate this image or be a bind mount: make sure the login
# shell's PATH covers ~/.local/bin, where the CLIs install.
if ! grep -qs '\.local/bin' /home/jodd-ai/.profile /home/jodd-ai/.bash_profile 2>/dev/null; then
  # Written AS THE ACCOUNT, never as root: the account controls its home, and a root append (or
  # chown) would follow a link it planted, e.g. ~/.profile -> /usr/local/bin/jodd-ai-host. env -i:
  # su would hand root's environment to the account's child.
  # shellcheck disable=SC2016  # the $ is for the account's shell
  env -i PATH=/usr/bin:/bin HOME=/home/jodd-ai su -s /bin/sh jodd-ai -c 'printf "%s\n" '"'"'export PATH="$HOME/.local/bin:$PATH"'"'"' >> /home/jodd-ai/.profile' \
    || echo "jodd-ai-host: could not add ~/.local/bin to the AI account's ~/.profile (is it a link?). Check: jodd-ai-host doctor" >&2
fi

# Password. Container filesystems are rebuilt on recreate but not on restart, so the rules are:
#  - never touch a password that is already set, unless JODD_AI_PASSWORD was deliberately changed;
#  - JODD_AI_PASSWORD is applied when the account has no password yet, or when it differs from the
#    value last applied from the environment (kept as a salted sha256 in the state volume);
#  - with no JODD_AI_PASSWORD, a password is generated only while the account has none, and shown once.
set_password() { printf 'jodd-ai:%s\n' "$1" | chpasswd; }
has_password() { [ "$(passwd -S jodd-ai 2>/dev/null | awk '{print $2}')" = P ]; }
envfile=$state/env-password.sha256
rm -f "$state/initial-password"   # older images kept the generated password here, in plain text
case "${JODD_AI_PASSWORD:-}" in
  '')
    if ! has_password; then
      pw=$(head -c 18 /dev/urandom | base64 | tr -d '/+=' | head -c 20)
      set_password "$pw"
      echo "jodd-ai-host: generated a password for jodd-ai: $pw  (shown only now; if you miss it, set a new one: docker compose exec ai-host jodd-ai-host password)"
    fi ;;
  *'
'*)
    echo "jodd-ai-host: JODD_AI_PASSWORD contains a line break and was ignored; the account's password is unchanged. Use a single-line password (in a .env file, write a literal \$ as \$\$)." ;;
  *)
    old=$(cat "$envfile" 2>/dev/null || :)
    salt=${old%%:*}
    [ -n "$old" ] || salt=$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')
    h=$(printf '%s%s' "$salt" "$JODD_AI_PASSWORD" | sha256sum | cut -d' ' -f1)
    if ! has_password || [ "$old" != "$salt:$h" ]; then
      set_password "$JODD_AI_PASSWORD"
      ( umask 077; printf '%s:%s' "$salt" "$h" > "$envfile" )
    fi ;;
esac

# CLIs: install on first start, in the background so sshd is up at once.
if [ ! -x /home/jodd-ai/.local/bin/claude ] || ! su -l jodd-ai -s /bin/sh -c 'command -v codex' >/dev/null 2>&1; then
  ( echo "jodd-ai-host: installing claude and codex (first start)…"
    jodd-ai-host update-clis && echo "jodd-ai-host: CLIs installed. Sign in: docker compose exec ai-host jodd-ai-host signin claude | codex" \
      || echo "jodd-ai-host: could not install the CLIs. Retry: docker compose exec ai-host jodd-ai-host update-clis" ) &
fi
if ! fp=$(jodd-ai-host fingerprint); then
  echo "jodd-ai-host: cannot read the host key's fingerprint from $key.pub." >&2
  exit 1
fi
echo "jodd-ai-host: fingerprint: $fp"
if ! /usr/sbin/sshd -t; then
  echo "jodd-ai-host: sshd rejected its configuration (message above), so it was not started." >&2
  echo "jodd-ai-host: the setting Jodd manages lives in the state volume: $state/50-jodd-ai.conf." >&2
  echo "jodd-ai-host: reset it with: docker compose exec ai-host jodd-ai-host unlock   (if the container keeps stopping, remove that file from the volume; it is recreated on the next start)." >&2
  exit 1
fi
echo "jodd-ai-host ready — in Jodd: App settings → LLM provider → Agent CLI (headless) → Runs on: A server; user jodd-ai, port ${JODD_AI_PORT:-2222}"
exec /usr/sbin/sshd -D -e
