#!/bin/sh
# Watch the route table and delete Colima's eth0 default route whenever it
# reappears. Colima's host-side agent re-adds the route post-boot at
# variable timings, so a one-shot fix at boot isn't enough.
set -u
exec /usr/sbin/ip monitor route | while IFS= read -r line; do
	case "$line" in
		Deleted*) continue ;;
		*"default via 192.168.5.2 dev eth0"*)
			/usr/sbin/ip route delete default via 192.168.5.2 dev eth0 2>/dev/null || true
			;;
	esac
done
