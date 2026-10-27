# Throwaway OpenSSH server for the AI-host proof (docs/BACKEND-SSH.md, "AI host").
# Alpine on purpose: busybox `ash` is a third `sh` (after dash and bash) for the
# generated wrapper, and its sshd runs the forced command through the user's
# login shell exactly like a small VPS would.
FROM alpine:latest
RUN apk add --no-cache openssh-server bash coreutils \
 && ssh-keygen -A \
 && adduser -D -s /bin/bash jodd \
 && echo 'jodd:jodd-test-pw' | chpasswd \
 && sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config \
 && sed -i 's/^#\?KbdInteractiveAuthentication.*/KbdInteractiveAuthentication no/' /etc/ssh/sshd_config \
 && mkdir -p /home/jodd/.local/bin \
 && printf '%s\n' 'export PATH="$HOME/.local/bin:$PATH"' > /home/jodd/.profile
COPY fake-codex /home/jodd/.local/bin/codex
COPY fake-claude /home/jodd/.local/bin/claude
RUN chmod 755 /home/jodd/.local/bin/* && chown -R jodd:jodd /home/jodd
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
