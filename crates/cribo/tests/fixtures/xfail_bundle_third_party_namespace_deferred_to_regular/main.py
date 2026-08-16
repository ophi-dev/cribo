"""A namespace portion defers to a regular installed package.

The entry directory carries a PEP 420 portion `duo/` (no __init__.py), but
the active environment installs a REGULAR `duo` package shipping a native
extension. Python's scan lets the regular package win, so classification must
apply the installed distribution's policies (native artifacts keep it
external as a whole) instead of inlining the portion's source first-party.
"""

from duo import core

print(core.WHO)
