"""A lambda walrus target is local for the WHOLE lambda body.

Reading `importlib` before the walrus that rebinds it raises
UnboundLocalError; the read must not be rewritten as the enclosing import.
"""

import importlib

probe = lambda: (importlib.import_module("helper"), (importlib := "replacement"))[1]  # noqa: E731

try:
    outcome = probe()
except UnboundLocalError:
    outcome = "unbound"

print(outcome)
