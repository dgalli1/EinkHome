#!/bin/sh
#
# uninstall-device.sh — remove the pbemu custom bookshelf + startup wrapper.
#
# Usage:
#   bookshelf/uninstall-device.sh <device-ip>
#
# Removes:
#   /mnt/ext1/system/bin/bookshelf.app   (startup wrapper → restores stock boot)
#   /mnt/ext1/applications/books.app     (custom bookshelf binary)
#   /mnt/ext1/applications/bookshelf.cfg (config)
#   /mnt/ext1/applications/bookshelf.log (log)
#   /mnt/ext1/applications/bookshelf-wrapper.log (wrapper log)
#   /tmp/pbemu_bookshelf.pid             (pid file, if present)
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

echo "==> removing custom bookshelf + wrapper from ${DEVICE}"

ssh ${SSH_COMMON} "root@${DEVICE}" sh -c '
	set -e
	# Kill running custom bookshelf.
	killall books.app 2>/dev/null || true

	# Remove the startup wrapper (restores stock boot path).
	rm -f /mnt/ext1/system/bin/bookshelf.app

	# Remove app files.
	rm -f /mnt/ext1/applications/books.app
	rm -f /mnt/ext1/applications/bookshelf.cfg
	rm -f /mnt/ext1/applications/bookshelf.log
	rm -f /mnt/ext1/applications/bookshelf-wrapper.log

	# Remove pid file.
	rm -f /tmp/pbemu_bookshelf.pid

	echo "done."
'

echo "==> uninstalled.  Reboot the device to restore the stock bookshelf."
