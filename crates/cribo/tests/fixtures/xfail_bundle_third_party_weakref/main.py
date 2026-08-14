"""Providers passed to weakref keep weak-referenceable module identity.

Real modules support weak references; a generated SimpleNamespace raises
TypeError, so the observed provider stays installed.
"""

import weakref

import wr_pkg

reference = weakref.ref(wr_pkg)
print("alive:", reference() is wr_pkg)
