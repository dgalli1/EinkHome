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


