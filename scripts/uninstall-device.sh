#!/bin/sh
#
# uninstall-device.sh — remove the pbemu custom bookshelf + startup wrapper.
#
# Usage:
#   scripts/uninstall-device.sh <device-ip>
#
# Removes:
#   /mnt/ext1/system/bin/bookshelf.app   (home-task binary → restores stock boot)
#   /mnt/ext1/system/bin/bookshelf.cfg   (config)
#   /mnt/ext1/applications/books.app     (legacy custom bookshelf binary)
#   /mnt/ext1/applications/bookshelf.cfg (legacy config)
#   /mnt/ext1/applications/bookshelf.log (log)
#   /mnt/ext1/applications/bookshelf-wrapper.log (log of the legacy wrapper
#                                                script — scripts/legacy/bookshelf-wrapper.sh)
#   /tmp/pbemu_bookshelf.pid             (legacy pid file, if present)
#
# After removal, reboot the device to restore the stock bookshelf as
# the home/startup app.

set -eu

SSH_COMMON='-o BatchMode=yes -o HostKeyAlgorithms=+ssh-rsa'

DEVICE="${1:-}"
if [ -z "${DEVICE}" ] || [ "${DEVICE}" = "-h" ] || [ "${DEVICE}" = "--help" ]; then
	cat >&2 <<EOF
usage: $(basename "$0") <device-ip>

Removes the custom bookshelf and startup wrapper from the device.
Reboot afterwards to restore the stock home screen.
EOF
	exit 64
fi

# Sanity-check ssh connectivity.
if ! ssh ${SSH_COMMON} -o ConnectTimeout=5 "root@${DEVICE}" true 2>/dev/null; then
	echo "ERROR: cannot ssh to root@${DEVICE} non-interactively." >&2
	echo "       run 'ssh-copy-id root@${DEVICE}' once before invoking this script." >&2
	exit 1
fi

echo "==> removing custom bookshelf from ${DEVICE}"

ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
	set -e
	# Kill running custom bookshelf.
	killall bookshelf.app 2>/dev/null || true
	killall books.app 2>/dev/null || true

	# Remove the home-task override (restores stock boot path).
	rm -f /mnt/ext1/system/bin/bookshelf.app
	rm -f /mnt/ext1/system/bin/bookshelf.cfg

	# Remove legacy app files (incl. the wrapper-script log — the
	# wrapper itself now lives at scripts/legacy/bookshelf-wrapper.sh).
	rm -f /mnt/ext1/applications/books.app
	rm -f /mnt/ext1/applications/bookshelf.cfg
	rm -f /mnt/ext1/applications/bookshelf.log
	rm -f /mnt/ext1/applications/bookshelf-wrapper.log

	# Remove pid file.
	rm -f /tmp/pbemu_bookshelf.pid

	echo "done."
'

echo "==> uninstalled.  Reboot the device to restore the stock bookshelf."
