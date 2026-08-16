"""A def rebinds an import alias: later attribute calls are not real imports.

After `def importlib(): ...`, `importlib.import_module` is an attribute lookup
on the function object and raises AttributeError; bundling must neither
rewrite the call nor bundle its target.
"""

import importlib


def importlib():
    return "not the module"


try:
    importlib.import_module("untouched_helper")
except AttributeError:
    print("AttributeError preserved")
