"""Ancestor packages of preserved dotted imports must reinitialize like modules.

The machinery imports `pkg` before `pkg.sub`, making the parent dynamically
importable too: importlib.reload must re-execute it over the RETAINED
dictionary, and a re-import after sys.modules eviction must produce a fresh
module object without the previous life's mutations.
"""

import importlib
import sys

options = {}
sub = importlib.import_module("pkg.sub", **options)
print("sub:", sub.VALUE)

parent = sys.modules["pkg"]
parent.mutated = "left-over"
reloaded = importlib.reload(parent)
print("reload keeps dict:", reloaded.mutated, reloaded is parent, reloaded.BANNER)

del sys.modules["pkg"]
del sys.modules["pkg.sub"]
fresh = importlib.import_module("pkg.sub", **options)
fresh_parent = sys.modules["pkg"]
print("fresh:", hasattr(fresh_parent, "mutated"), fresh_parent is parent, fresh_parent.BANNER)
