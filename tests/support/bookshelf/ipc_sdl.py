"""Headless IPC client for the SDL (PC) build of EinkHome.

Drives the app through the UNIX-socket control plane in
app/platform/eh_plat_sdl.c — no emulator, no window.  The method names
mirror the pbemu `BookshelfSession` (tests/support/bookshelf/session.py)
so the same test logic can target either backend.

Protocol: newline-delimited commands, one-line text replies.

    tap x y / down x y / up x y / move x y
    key <0xIVKEY|dec>
    type TEXT
    hash            -> "hash=0x%016llx"
    shot PATH       -> P6 PPM of the canvas
    state           -> "state=<overlay>:<tab>:<page>"
    quit

The socket path must match the running app's $EH_SOCKET.  Parallel test
runs each launch their own app with a distinct socket → no collision.
"""

import os
import pathlib
import socket
import subprocess
import time

# Same IV_KEY codes as the emulator's ui_input.py
IV_KEY_MENU = 0x17
IV_KEY_PREV = 0x18
IV_KEY_NEXT = 0x19
IV_KEY_HOME = 0x1A
IV_KEY_BACK = 0x1B
IV_KEY_PREV2 = 0x1C
IV_KEY_NEXT2 = 0x1D


class IpcBookshelf:
    """Connect to a running SDL bookshelf.pc via its control socket."""

    def __init__(self, sock_path: str, *, timeout: float = 5.0):
        self.sock_path = sock_path
        self.timeout = timeout
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.settimeout(timeout)
        self._sock.connect(sock_path)
        self._buf = b""

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass

    def __enter__(self) -> "IpcBookshelf":
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    # -- low-level ------------------------------------------------------

    def cmd(self, line: str) -> str:
        """Send one command line and return the (first) reply line."""
        self._sock.sendall((line + "\n").encode())
        while b"\n" not in self._buf:
            chunk = self._sock.recv(4096)
            if not chunk:
                raise ConnectionError(f"app closed socket (cmd={line!r})")
            self._buf += chunk
        line_b, self._buf = self._buf.split(b"\n", 1)
        return line_b.decode("utf-8", "replace").strip()

    # -- event injection ------------------------------------------------

    def tap_at(self, x: int, y: int) -> None:
        self.cmd(f"tap {x} {y}")
        time.sleep(0.2)  # let the app draw the result before a hash/shot

    def pointer_down(self, x: int, y: int) -> None:
        self.cmd(f"down {x} {y}")

    def pointer_move(self, x: int, y: int) -> None:
        self.cmd(f"move {x} {y}")

    def pointer_up(self, x: int, y: int) -> None:
        self.cmd(f"up {x} {y}")

    def long_press_at(self, x: int, y: int, *, hold: float = 0.9) -> None:
        self.pointer_down(x, y)
        time.sleep(hold)
        self.pointer_up(x, y)
        time.sleep(0.3)

    def key(self, iv_key: int) -> None:
        self.cmd(f"key 0x{iv_key:x}")

    def type_text(self, text: str) -> None:
        self.cmd(f"type {text}")

    # -- query ----------------------------------------------------------

    def frame_hash(self) -> str:
        """FNV1a-64 of the RGBA canvas — same algorithm as pbemu's
        frame_dump --hash, so hashes are comparable across backends."""
        reply = self.cmd("hash")
        return reply.split("=", 1)[1] if "=" in reply else reply

    def wait_hash_change(self, baseline: str, *, timeout: float = 8.0) -> str:
        """Poll until the frame hash differs from *baseline* (stable)."""
        deadline = time.monotonic() + timeout
        seen: list[str] = []
        while time.monotonic() < deadline:
            h = self.frame_hash()
            if h != baseline:
                if not seen or seen[-1] == h:
                    if h in seen:
                        return h  # settled on a new value
                    seen.append(h)
                else:
                    seen[-1] = h
            time.sleep(0.1)
        raise AssertionError(
            f"frame hash did not change from {baseline} within {timeout}s; "
            f"seen={seen}"
        )

    def state(self) -> tuple[int, int, int]:
        """Return (overlay, tab, page)."""
        reply = self.cmd("state")
        # state=overlay:tab:page
        _, vals = reply.split("=", 1)
        a, b, c = vals.split(":")
        return int(a), int(b), int(c)

    def shot(self, path: str) -> None:
        self.cmd(f"shot {path}")

    def quit(self) -> None:
        try:
            self.cmd("quit")
        except (ConnectionError, OSError):
            pass
        self.close()

    # -- convenience (mirror BookshelfSession geom helpers) ------------

    def _center_of(self, x0, y0, x1, y1) -> tuple[int, int]:
        return (x0 + x1) // 2, (y0 + y1) // 2

    def sync_button(self) -> tuple[int, int]:
        """Top bar sync (refresh) icon — the 3rd icon from the right
        (search, layout, sync, menu).  Right stack starts at
        w-(8+4*96)=680, each 96px wide: sync is 872-968 → center (920,48)."""
        w = 1072
        first = w - (8 + 4 * 96)
        return self._center_of(first + 2 * 96, 0, first + 3 * 96, 96)

    def menu_button(self) -> tuple[int, int]:
        """Top bar '...' button (rightmost, 4th from right)."""
        w = 1072
        first = w - (8 + 4 * 96)
        return self._center_of(first + 3 * 96, 0, first + 4 * 96, 96)


def launch_headless(
    binary: str,
    sock_path: str,
    api_url: str = "http://127.0.0.1:8765",
    *,
    wait_ready: float = 5.0,
) -> "subprocess.Popen":
    """Launch bookshelf.pc headless with a control socket; returns the
    Popen.  Caller owns cleanup (proc.terminate())."""
    env = os.environ.copy()
    env["EH_SOCKET"] = sock_path
    env["SDL_VIDEODRIVER"] = "dummy"  # no window/display needed
    env.setdefault("EH_API_URL", api_url)
    proc = subprocess.Popen([binary], env=env)
    # Wait for the socket to appear.
    deadline = time.monotonic() + wait_ready
    while time.monotonic() < deadline:
        if pathlib.Path(sock_path).exists():
            return proc
        if proc.poll() is not None:
            raise RuntimeError(f"bookshelf.pc exited early: rc={proc.returncode}")
        time.sleep(0.1)
    proc.terminate()
    raise TimeoutError(f"control socket {sock_path} not ready")
