"""Repeated eviction re-imports must not grow the pristine snapshot store.

Every fresh eviction life gets a new namespace object; recording a snapshot
per life would retain memory unboundedly in hot-reload processes (and risk
id-reuse collisions). Only the long-lived registered object needs one.
"""

import importlib
import sys

options = {}
counter = importlib.import_module("counter", **options)

lives = []
for _ in range(3):
    # Keep each evicted life alive, like a plugin registry would: the store
    # must stay bounded even when old module objects remain referenced
    lives.append(sys.modules["counter"])
    del sys.modules["counter"]
    importlib.import_module("counter", **options)

sizes = []
for finder in sys.meta_path:
    loader = getattr(finder, "_loader", None)
    pristine = getattr(loader, "_pristine", None)
    if pristine is not None:
        sizes.append(len(pristine))
print("bounded:", all(size <= 2 for size in sizes))
