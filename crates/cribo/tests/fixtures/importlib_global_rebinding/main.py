"""A `global`-declared alias resolves through the module scope, not a shadow.

The function declares `global importlib`, calls import_module BEFORE the
rebinding, and only then reassigns: the call resolves through the module-level
import, so `helper` must be discovered and bundled.
"""

import importlib


def load():
    global importlib
    module = importlib.import_module("helper")
    importlib = None
    return module


print("VALUE:", load().VALUE)
