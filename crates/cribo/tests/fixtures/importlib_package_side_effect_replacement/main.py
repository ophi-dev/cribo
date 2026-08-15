"""A package expression mutating sys.modules must affect what import returns.

Python evaluates the package argument BEFORE consulting sys.modules, so a
helper replacing the sys.modules entry makes import_module return the
replacement; an evaluate-then-direct-access rewrite would wrongly yield the
bundled module. Calls whose package expression is not provably inert must
stay on the real import path.
"""

import importlib
import sys
import types


def replace_entry():
    replacement = types.ModuleType("target")
    replacement.VALUE = "replacement"
    sys.modules["target"] = replacement
    return "ignored"


loaded = importlib.import_module("target", package=replace_entry())
print(loaded.VALUE)
