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
