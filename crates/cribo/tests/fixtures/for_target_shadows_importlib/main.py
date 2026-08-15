"""A for-loop iterable evaluates BEFORE the target rebinds the name.

The import_module call inside the iterable still dispatches through the
imported importlib module, so its literal target must be discovered and
rewritten; only the loop BODY observes the rebound name.
"""

import importlib

for importlib in [importlib.import_module("helper")]:
    print(importlib.KIND)
