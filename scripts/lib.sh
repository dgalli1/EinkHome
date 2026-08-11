# lib.sh — shared helpers for the EinkHome scripts.
#
# Sourced by run.sh, run-visible.sh and install-device.sh.  Must stay
# POSIX-sh compatible: the callers run under `set -eu` with /bin/sh.

# lan_ip — print the host's primary LAN IPv4 address.
#
# Picks the first non-loopback IPv4 with a default route
# (`ip -4 -o addr show scope global`); falls back to the first
# non-loopback address reported by `hostname -I`.  When neither yields
# an address, prints PBEMU_LAN_FALLBACK if set, else the hard-coded
# 192.168.1.42 (with a warning on stderr), so callers always get a
# value.  Override the magic fallback per-invocation with:
#     PBEMU_LAN_FALLBACK=10.0.0.5 ./scripts/run.sh
lan_ip() {
	_lan_ip=$(
		ip -4 -o addr show scope global 2>/dev/null |
			awk '{print $4}' |
			cut -d/ -f1 |
			head -n1
	)
	if [ -z "${_lan_ip}" ]; then
		_lan_ip=$(
			hostname -I 2>/dev/null |
				tr ' ' '\n' |
				grep -v '^127\.' |
				head -n1
		)
	fi
	if [ -z "${_lan_ip}" ]; then
		_lan_ip="${PBEMU_LAN_FALLBACK:-192.168.1.42}"
		echo "WARN: could not detect LAN ip; falling back to ${_lan_ip}" >&2
	fi
	printf '%s\n' "${_lan_ip}"
}
