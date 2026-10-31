#!/usr/bin/env python3
"""Install DEMIURGE's Wayland stack: The shared installer (installer.py at the
repository's root) for the wayland stack."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import installer  # noqa: E402

if __name__ == "__main__":
    installer.run("wayland")
