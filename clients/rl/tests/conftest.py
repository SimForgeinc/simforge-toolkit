from __future__ import annotations

import shutil
import tempfile
from pathlib import Path

import pytest


@pytest.fixture()
def tmp_path():
    """A short directory: Unix socket paths are capped near 108 bytes."""
    path = Path(tempfile.mkdtemp(prefix="sfrl-", dir="/tmp"))
    yield path
    shutil.rmtree(path, ignore_errors=True)
