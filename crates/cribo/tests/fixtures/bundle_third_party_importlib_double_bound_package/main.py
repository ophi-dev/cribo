import importlib

import dbl_pkg

try:
    importlib.import_module("dbl_pkg", "context", package="other")
except TypeError:
    # ``package`` is bound twice; Python raises before importing anything and
    # bundling must preserve that behavior
    print("TypeError preserved")

print(dbl_pkg.VALUE)
