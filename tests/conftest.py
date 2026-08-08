"""Fixtures for the rewritten PocketBook emulator test suite."""

from __future__ import annotations

import os
import shutil
import sys
from collections.abc import Iterator
from pathlib import Path

import pytest

_REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(_REPO_ROOT / "tools"))

# Imports are placed after the sys.path mutation because pbemu lives under
# ``tools/`` rather than as an installed package during interactive runs.
# pylint: disable=wrong-import-position
from tests.support.reader_flow import return_to_home_screen  # noqa: E402
from tests.support.runtime import (  # noqa: E402
    CONTAINER,
    PODMAN,
    Emulator,
    container_running,
    container_sh,
    detect_firmware,
)
# pylint: enable=wrong-import-position

del CONTAINER  # imported for re-export discovery only

_READER_APP = "/workspace/firmware/ebrmain/bin/eink-reader.app"
_CUSTOM_HOME_APP = "/mnt/ext1/system/bin/bookshelf.app"


def pytest_collection_modifyitems(config, items):
    """Skip integration/bookshelf tests when env vars are set."""
    del config
    skip_int = os.environ.get("PB_SKIP_INTEGRATION") == "1"
    skip_bs = os.environ.get("PB_SKIP_BOOKSHELF") == "1"
    if not skip_int and not skip_bs:
        return
    for item in items:
        if skip_int:
            item.add_marker(pytest.mark.skip(reason="PB_SKIP_INTEGRATION=1"))
        elif skip_bs and item.get_closest_marker("bookshelf") is not None:
            item.add_marker(pytest.mark.skip(reason="PB_SKIP_BOOKSHELF=1"))


def pytest_configure(config) -> None:
    """Register local pytest markers used by the rewritten test suite."""
    config.addinivalue_line(
        "markers",
        "no_home_reset: skip the autouse reset-to-home fixture for tests "
        "that do not depend on home-surface state",
    )
    config.addinivalue_line(
        "markers",
        "bookshelf: e2e tests for the bookshelf replacement app",
    )


def _boot_emulator(firmware: str) -> Emulator:
    if shutil.which(PODMAN) is None:
        pytest.skip(f"{PODMAN} not available on PATH")

    external = os.environ.get("PB_EMULATOR_EXTERNAL") == "1"
    ready_timeout = 20 if external else 120
    quiet_period = 0.5 if external else 1.5

    emulator = Emulator(firmware=firmware)
    emulator.start()
    try:
        _wait_ready_emulator(
            emulator,
            ready_timeout=ready_timeout,
            quiet_period=quiet_period,
        )
    except TimeoutError:
        emulator.stop(force=True)
        emulator.start(force=True)
        _wait_ready_emulator(
            emulator,
            ready_timeout=ready_timeout,
            quiet_period=quiet_period,
        )
    return emulator


def _wait_ready_emulator(
    emulator: Emulator,
    *,
    ready_timeout: float,
    quiet_period: float,
) -> None:
    """Wait for the emulator to expose a stable active framebuffer."""
    emulator.wait_for_monitor(timeout=ready_timeout)
    emulator.wait_for_hwevent(timeout=ready_timeout)
    emulator.wait_for_informer_snapshot(timeout=ready_timeout)
    emulator.wait_for_active_task_info(timeout=ready_timeout)
    emulator.wait_for_monitor_quiet(
        timeout=min(20.0, float(ready_timeout)),
        quiet_period=quiet_period,
    )


def _assert_reader_prerequisites() -> None:
    binary = container_sh(f"test -x {_READER_APP} && echo ok", check=False)
    assert "ok" in binary.stdout, "eink-reader.app missing from firmware"
    custom = container_sh(
        f"test -x {_CUSTOM_HOME_APP} && echo yes", check=False
    )
    if "yes" in custom.stdout:
        pytest.skip(
            "custom bookshelf home staged on the user partition: the stock "
            "recent-book open flow these tests drive is unavailable"
        )


@pytest.fixture(scope="session", name="_emulator_holder")
def _emulator_holder_fixture() -> Iterator[dict]:
    """Session-lifetime holder for the shared emulator instance."""
    holder: dict = {}
    try:
        yield holder
    finally:
        instance = holder.get("instance")
        if instance is not None:
            instance.stop()


def _shared_emulator(holder: dict) -> Emulator:
    """Return the shared emulator, rebooting it if a module (e.g. the
    bookshelf e2e suite, which runs the emulator with its own flags and
    stops it on module teardown) tore the container down."""
    instance = holder.get("instance")
    if instance is None or not container_running():
        instance = _boot_emulator(detect_firmware())
        holder["instance"] = instance
    return instance


@pytest.fixture(name="emulator")
def _emulator_fixture(_emulator_holder: dict) -> Emulator:
    """Provide the shared headless emulator, self-healing across modules."""
    return _shared_emulator(_emulator_holder)


@pytest.fixture(scope="module", name="emulator_reader")
def _emulator_reader_fixture(_emulator_holder: dict) -> Emulator:
    """Shared emulator with reader prerequisites verified per module."""
    instance = _shared_emulator(_emulator_holder)
    _assert_reader_prerequisites()
    return instance


@pytest.fixture(autouse=True)
def reset_to_home_screen(request):
    """Return to the home screen before each rewritten live-emulator test."""
    if _should_skip_home_reset(request):
        yield
        return

    emulator_instance: Emulator = request.getfixturevalue("emulator")
    try:
        return_to_home_screen(emulator_instance, timeout=12.0)
    except (RuntimeError, TimeoutError):
        pass
    yield


def _should_skip_home_reset(request) -> bool:
    """Return True when the autouse home-reset fixture should do nothing."""
    return (
        request.node.get_closest_marker("no_home_reset") is not None
        or "emulator_reader" in request.fixturenames
        or "emulator" not in request.fixturenames
    )
