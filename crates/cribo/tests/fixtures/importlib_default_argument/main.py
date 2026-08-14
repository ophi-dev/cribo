"""Default-argument expressions evaluate in the ENCLOSING scope.

The parameter name shadows `importlib` only inside the body; the default
expression `importlib.import_module(...)` runs at definition time through
the module-level import, so `effectful_helper` must be discovered and
bundled.
"""

import importlib


def load(importlib=importlib.import_module("effectful_helper")):
    return importlib


helper = load()
print("loaded:", helper.GREETING)
