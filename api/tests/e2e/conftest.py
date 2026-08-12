"""E2E test fixtures for the Kavita API integration.

These tests hit a real Kavita server — they do NOT mock anything.
Skipped automatically when KAVITA_E2E_URL / KAVITA_E2E_API_KEY aren't set, so
`pytest api/tests/` stays fast and offline by default.

To run against the live server:

    export KAVITA_E2E_URL=https://kavita.example.com
    export KAVITA_E2E_API_KEY=74241d5e-...
    # optional — fall back to username/password if no api key:
    export KAVITA_E2E_USER=alice
    export KAVITA_E2E_PASS=secret
    pytest api/tests/e2e/ -v

The tests exercise:
  - KavitaProvider directly (covers the adapter unit-by-unit)
  - The full HTTP server pointed at Kavita (covers the wire-format
    that the on-device C app actually sees)

Nothing deletes data.  All writes are limited to /api/Account/login
(login is read-only on the user side; tokens are not persisted) and
POST /api/v1/sync/state (which the server records but the upstream
ignores — that's the only thing our schema exposes as a write).
"""

# pylint: disable=missing-function-docstring,redefined-outer-name
import os
import sys

import pytest

REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if REPO_ROOT not in sys.path:
    sys.path.insert(0, REPO_ROOT)
if os.path.join(REPO_ROOT, "api") not in sys.path:
    sys.path.insert(0, os.path.join(REPO_ROOT, "api"))

# KAVITA_E2E_URL is the canonical "skip me when unset" knob.
KAVITA_URL = os.environ.get("KAVITA_E2E_URL")
KAVITA_API_KEY = os.environ.get("KAVITA_E2E_API_KEY")
KAVITA_USER = os.environ.get("KAVITA_E2E_USER", "")
KAVITA_PASS = os.environ.get("KAVITA_E2E_PASS", "")
KAVITA_TIMEOUT = float(os.environ.get("KAVITA_E2E_TIMEOUT", "60"))

# Reasons we skip instead of failing:
SKIP_NO_URL = pytest.mark.skipif(
    not KAVITA_URL,
    reason="KAVITA_E2E_URL is not set; skipping live Kavita tests",
)
SKIP_NO_AUTH = pytest.mark.skipif(
    not KAVITA_URL or not (KAVITA_API_KEY or (KAVITA_USER and KAVITA_PASS)),
    reason=(
        "live Kavita tests need KAVITA_E2E_URL + either "
        "KAVITA_E2E_API_KEY or KAVITA_E2E_USER+KAVITA_E2E_PASS"
    ),
)


# Reachability is deliberately NOT probed at collection time: a transient
# network blip at import used to silently skip the whole live suite (green
# CI, zero coverage).  Env presence is the only thing we evaluate here; the
# actual reachability/auth check happens lazily in the module fixtures, where
# a configured-but-unreachable host fails loudly instead of skipping.
SKIP_UNREACHABLE = pytest.mark.skipif(
    not KAVITA_URL,
    reason="KAVITA_E2E_URL is not set; skipping live Kavita tests",
)


@pytest.fixture(scope="session")
def kavita_url():
    return KAVITA_URL or ""


@pytest.fixture(scope="session")
def kavita_provider_cfg():
    return {
        "base_url": KAVITA_URL or "",
        "api_key": KAVITA_API_KEY,
        "username": KAVITA_USER,
        "password": KAVITA_PASS,
        "timeout": KAVITA_TIMEOUT,
    }
