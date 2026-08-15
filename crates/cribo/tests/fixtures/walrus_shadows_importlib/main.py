"""A walrus target rebinds its name for all SUBSEQUENT expressions.

The first import_module call dispatches through the real importlib (the
walrus in the same statement list has not executed yet), while calls after
the walrus dispatch to the custom binding and must stay untouched.
"""

import importlib

real = importlib.import_module("helper")
print("real:", real.KIND)

(importlib := real)
print("custom:", importlib.import_module("helper"))
