FROM debian:bookworm-slim

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates \
      coreutils \
      git \
      iproute2 \
      openssh-server \
      tmux \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash ade \
    && passwd -d ade \
    && install -d -m 0755 /run/sshd \
    && printf '%s\n' \
      'PasswordAuthentication no' \
      'KbdInteractiveAuthentication no' \
      'PermitRootLogin no' \
      'AllowUsers ade' \
      'UsePAM no' \
      'Subsystem sftp internal-sftp' \
      > /etc/ssh/sshd_config

COPY entrypoint.sh /usr/local/bin/phase0-ssh-entrypoint

EXPOSE 22
ENTRYPOINT ["/usr/local/bin/phase0-ssh-entrypoint"]

