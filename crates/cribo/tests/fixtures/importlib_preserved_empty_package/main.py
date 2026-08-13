"""A relative import_module with an empty package anchor keeps CPython's TypeError.

An empty string is falsy: CPython refuses the relative import before touching
any import machinery, so the call must stay verbatim instead of resolving to a
bundled helper.
"""

import importlib

import helper

try:
    importlib.import_module(".helper", package="")
except TypeError:
    print("TypeError raised for empty package anchor")

print("helper:", helper.VALUE)
