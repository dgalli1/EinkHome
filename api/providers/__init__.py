"""
api/providers/__init__.py — content provider adapters.

Each provider implements the `Provider` interface declared in `base.py`.
Adding a new provider is a matter of dropping a `providers/<name>.py`
file that subclasses `Provider` and registers itself in
`api/server.py:_build_provider`.
"""

from .base import (
    AuthorInfo,
    BookMeta,
    LibraryInfo,
    Provider,
    SeriesInfo,
)

__all__ = [
    "AuthorInfo",
    "BookMeta",
    "LibraryInfo",
    "Provider",
    "SeriesInfo",
]
