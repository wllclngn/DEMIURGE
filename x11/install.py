#!/usr/bin/env python3
"""Install DEMIURGE's X11 stack: The shared installer (installer.py at the
repository's root) for the x11 stack."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import installer  # noqa: E402

if __name__ == "__main__":
    installer.run("x11")
