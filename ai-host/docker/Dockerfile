# Jodd AI host: OpenSSH plus a dedicated account for the claude / codex CLIs.
# The CLIs are NOT in this image: they install from their official installers
# into the account's home volume on first start, on your machine.
# Guide: https://github.com/BBM-Co-ORG/Jodd-public/blob/main/docs/ai-host/INSTALL.md
FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/BBM-Co-ORG/Jodd-public" \
      org.opencontainers.image.description="AI host for Jodd: run your own claude / codex CLI over a restricted SSH key" \
      org.opencontainers.image.licenses="Apache-2.0"
RUN apt-get update \
 && apt-get install -y --no-install-recommends openssh-server openssh-client bash curl ca-certificates procps util-linux coreutils ripgrep netcat-openbsd \
 && rm -rf /var/lib/apt/lists/* /etc/ssh/ssh_host_* \
 && useradd -m -s /bin/bash jodd-ai \
 && printf '%s\n' 'export PATH="$HOME/.local/bin:$PATH"' >> /home/jodd-ai/.profile \
 && mkdir -p /run/sshd /var/lib/jodd-ai-host /usr/local/lib/jodd-ai-host
COPY lib/ /usr/local/lib/jodd-ai-host/
COPY bin/jodd-ai-host /usr/local/bin/jodd-ai-host
COPY docker/entrypoint.sh /usr/local/sbin/jodd-ai-host-entrypoint
RUN chmod 755 /usr/local/bin/jodd-ai-host /usr/local/sbin/jodd-ai-host-entrypoint
EXPOSE 22
VOLUME ["/home/jodd-ai", "/var/lib/jodd-ai-host"]
ENTRYPOINT ["/usr/local/sbin/jodd-ai-host-entrypoint"]
