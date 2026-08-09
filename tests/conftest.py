"""Path bootstrap for the EinkHome test suite.

The emulator test support framework lives in the pbemu submodule
(tests/support) — not in this repository.  Put the submodule root on
sys.path so `tests.support.*` resolves there; the test files in this
repo stay self-contained and locate the app via EINKHOME_ROOT.
"""

import sys
from pathlib import Path

_PBEMU = Path(__file__).resolve().parents[1] / "pbemu"
sys.path.insert(0, str(_PBEMU))
