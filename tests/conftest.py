"""Path bootstrap for the EinkHome test suite.

The generic emulator test framework lives in the pbemu submodule
(tests/support) — not in this repository.  Put the submodule root on
sys.path so `tests.support.*` resolves there; the test files in this
repo stay self-contained and locate the app via EINKHOME_ROOT.

The bookshelf-APP-specific harness layer (tests/support/bookshelf —
tap targets and session helpers that exist only to drive this app's
UI) lives HERE.  It is registered below as `tests.support.bookshelf`
in sys.modules, so the unchanged `tests.support.bookshelf` imports
keep working while the generic pieces still resolve from pbemu.
"""

import importlib.util
import os
import sys
from pathlib import Path

import pytest

_EINKHOME = Path(__file__).resolve().parents[1]
_PBEMU = _EINKHOME / "pbemu"
sys.path.insert(0, str(_PBEMU))

_BS_DIR = Path(__file__).resolve().parent / "support" / "bookshelf"
_bs_spec = importlib.util.spec_from_file_location(
    "tests.support.bookshelf",
    _BS_DIR / "__init__.py",
    submodule_search_locations=[str(_BS_DIR)],
)
_bs_mod = importlib.util.module_from_spec(_bs_spec)
sys.modules["tests.support.bookshelf"] = _bs_mod
_bs_spec.loader.exec_module(_bs_mod)

# Hosted runners can take >1s per `podman exec` probe (cold container,
# loaded VM), which makes pbemu's 1s active-task sampling timeout
# spuriously.  Raise it for the whole suite; `wait_for_active_app`
# reads the module global at call time.
import tests.support.reader.session as _reader_session  # noqa: E402

_reader_session.ACTIVE_TASK_SAMPLE_TIMEOUT = 5.0


def pytest_configure(config: pytest.Config) -> None:
    """Register the suite's pytest markers (silences PytestUnknownMark)."""
    config.addinivalue_line(
        "markers",
        "bookshelf: bookshelf e2e suite (emulator-backed, requires podman)",
    )
    # Report collector: per-test outcomes + screenshot steps ->
    # build/report/results.json (see tests/support/bookshelf/report.py).
    import tests.support.bookshelf.report as _report_mod

    config.pluginmanager.register(_report_mod)


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call) -> None:
    """Remember the call-phase report so fixtures can capture a FAILED
    screenshot on teardown (see fresh_bookshelf / scale_env)."""
    outcome = yield
    if call.when == "call":
        item._bs_call_report = outcome.get_result()  # type: ignore[attr-defined]


@pytest.hookimpl(trylast=True)
def pytest_collection_modifyitems(config, items):
    """Skip tests that require the device/emulator storage backend when
    running against the SDL (or other non-emulator) target.

    The interactive-UI tier (navigation, overlays, pager, sorting) runs on
    any backend.  The storage tier (downloads, reader launch, offline boot,
    series suggestion file injection, settings persistence via the device
    config) reaches into the .live filesystem / NewTaskEx / on-screen
    keyboard rendering, which are emulator/device concerns — those tests are
    skipped when BS_TEST_BACKEND is not the emulator.
    """
    backend = os.environ.get("BS_TEST_BACKEND", "emulator")
    if backend == "emulator":
        return
    _STORAGE_HELPERS = {
        "test_book_tap_launches_reader",
        "test_book_press_downloads_and_launches_reader",
        "test_download_all_opens_popup_and_drains",
        "test_download_all_drains_beyond_first_slice",
        "test_download_all_failures_finish_not_loop",
        "test_download_keeps_ui_responsive",
        "test_book_longpress_open",
        "test_book_longpress_download",
        "test_book_longpress_delete",
        "test_series_card_drill_in_and_back",
        "test_series_longpress_download_all",
        "test_series_longpress_delete",
        "test_offline_boot_renders_cached_library",
        "test_legacy_json_store_migrates_to_sqlite",
        "test_search_suggestions_live_and_commit",
        "test_search_folded_suggestion_finds_diacritic_title",
        "test_settings_reader_cycle_and_save",
        "test_settings_reader_pref_persists_across_restart",
        # Launcher app-launch asserts "NewTaskEx is called" — the SDL
        # backend launches a desktop binary instead and repaints nothing,
        # so the framebuffer never changes.
        "test_launcher_tap_app_launches_task",
        # Search-page keyboard-commit/suggestion tests rely on the
        # firmware's on-screen keyboard rendering / EVT_EXT_KB path.
        "test_search_tap_opens_keyboard",
        "test_search_commit_filters_grid",
        "test_search_history_persists_and_reruns",
        "test_search_keyboard_outside_tap_stays_on_search",
        "test_search_suggestions_live_and_commit",
        "test_no_crash_after_all_interactions",
    }
    sel = []
    for item in items:
        if item.name in _STORAGE_HELPERS:
            item.add_marker(
                pytest.mark.skipif(
                    True,
                    reason=(
                        "requires the emulator/device storage backend "
                        f"(BS_TEST_BACKEND={backend})"
                    ),
                )
            )
            sel.append(item)


