"""A later function-local import makes earlier uses unbound, exactly like Python.

`import json as importlib` at the end of the function makes `importlib` local
for the WHOLE body, so the earlier call raises UnboundLocalError; bundling must
neither rewrite the call nor bundle its target.
"""

import importlib


def broken():
    result = importlib.import_module("untouched_helper")
    import json as importlib

    return result, importlib


try:
    broken()
except UnboundLocalError:
    print("UnboundLocalError preserved")
