"""Repeated imports of inlined modules must not grow the snapshot store.

Namespace-less inlined registrations build a NEW module object on every
import; recording pristine snapshots for those would retain their
dictionaries unboundedly in hot-reload or repeated-pickle workflows.
"""

import importlib
import sys

from models import Token

module_name = "".join(["mod", "els"])
lives = []
for _ in range(3):
    lives.append(importlib.import_module(module_name))
    sys.modules.pop(module_name, None)

sizes = []
for finder in sys.meta_path:
    loader = getattr(finder, "_loader", None)
    pristine = getattr(loader, "_pristine", None)
    if pristine is not None:
        sizes.append(len(pristine))
print("bounded:", all(size <= 2 for size in sizes), lives[0].Token is Token)
