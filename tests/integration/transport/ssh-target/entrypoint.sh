#!/bin/sh
set -eu

install -d -m 0700 -o ade -g ade /home/ade/.ssh
install -m 0600 -o ade -g ade /config/authorized_keys /home/ade/.ssh/authorized_keys
ssh-keygen -A
exec /usr/sbin/sshd -D -e

