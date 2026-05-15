#!/usr/bin/env bash
# Install the in-VM networking-fix units inside Colima's default profile.
#
# Background. Colima's QEMU-on-macOS default profile installs two default
# routes in the VM: col0 (metric 100, via the macOS host — works) and
# eth0 (metric 200, via QEMU slirp at 192.168.5.2 — broken for the Docker
# daemon's outbound). The slirp DNS proxy at 192.168.5.1 is also flaky for
# daemon image-pull queries. Symptoms in plain Docker: random
# "network is unreachable" or "server misbehaving" errors on `docker pull`
# or `docker build`'s `FROM`.
#
# Units copied into the VM:
#   colima-fix-network.service   drops the eth0 default route at boot,
#                                writes /etc/gai.conf to prefer IPv4
#                                (the VM has no IPv6 transit)
#   colima-fix-resolv.service    rewrites /etc/resolv.conf to public DNS
#   colima-fix-resolv.path       re-fires the resolv service whenever
#                                Colima's host-side agent resets it
#   colima-route-watcher.sh      runs ip-monitor-route in a loop and
#                                deletes the eth0 default route every
#                                time Colima's agent re-adds it post-boot
#   colima-route-watcher.service Restart=always daemon for the watcher
#
# Re-run after `colima delete` + recreate. Idempotent.
#
# Usage:
#     tests/scripts/colima-vm-setup/install.sh

set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)

for unit in colima-fix-network.service colima-fix-resolv.service colima-fix-resolv.path colima-route-watcher.service; do
	echo "Installing $unit ..."
	colima ssh -- sudo tee "/etc/systemd/system/$unit" < "$SCRIPT_DIR/$unit" > /dev/null
done

echo "Installing colima-route-watcher.sh ..."
colima ssh -- sudo tee /usr/local/sbin/colima-route-watcher.sh < "$SCRIPT_DIR/colima-route-watcher.sh" > /dev/null
colima ssh -- sudo chmod +x /usr/local/sbin/colima-route-watcher.sh

colima ssh -- sudo systemctl daemon-reload
colima ssh -- sudo systemctl enable colima-fix-network.service colima-fix-resolv.path colima-route-watcher.service

echo
echo "Units installed and enabled. Restart Colima to apply on the next boot:"
echo "    colima stop && colima start"
echo
echo "Or apply immediately without restarting:"
echo "    colima ssh -- sudo systemctl start colima-fix-network.service colima-fix-resolv.path colima-route-watcher.service"
