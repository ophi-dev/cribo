"""Relative imports resolve through the selected package's own __path__.

A local namespace portion `relpkg/sub.py` (no __init__.py) loses to the
regular site-packages `relpkg`; that package's `from . import sub` must
then load the INSTALLED sub.py, never the local portion's file.
"""

import relpkg

print("value:", relpkg.VALUE)
