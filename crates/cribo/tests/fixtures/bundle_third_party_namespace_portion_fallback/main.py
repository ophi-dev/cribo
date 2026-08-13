"""A local PEP 420 namespace portion must not block the site-packages fallback.

The entry directory contains `pkg/` WITHOUT __init__.py (a namespace portion),
while site-packages provides a regular `pkg` package: Python keeps scanning and
the regular package wins, so `import pkg.sub` must resolve and bundle.
"""

import pkg.sub

print("VALUE:", pkg.sub.VALUE)
