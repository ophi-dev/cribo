"""Module-scope rebinding evaluates the RHS through the OLD import binding.

`importlib = importlib.import_module("helper")` resolves the call through the
real importlib import before rebinding the name, so `helper` must be
discovered and bundled.
"""

import importlib

importlib = importlib.import_module("helper")
print("VALUE:", importlib.VALUE)
