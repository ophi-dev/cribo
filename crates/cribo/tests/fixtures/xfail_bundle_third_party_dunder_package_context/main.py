"""Providers anchoring relative imports on __package__ stay external.

The literal ".helper" target is only resolvable when the package context is
a literal too; `__package__` is a runtime value, so the provider keeps its
installed distribution.
"""

import anchored_pkg

print("value:", anchored_pkg.backend.VALUE)
