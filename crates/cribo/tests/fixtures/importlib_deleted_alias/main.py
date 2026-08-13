"""del kills an import alias: later uses raise NameError, exactly like Python.

Bundling must neither rewrite the call after the delete nor bundle its target.
"""

import importlib

del importlib

try:
    importlib.import_module("untouched_helper")
except NameError:
    print("NameError preserved")
