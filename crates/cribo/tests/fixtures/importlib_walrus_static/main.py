"""Static import calls nested in walrus expressions are rewritten.

The named expression's value must be transformed like any other call site.
"""

import importlib

if (mod := importlib.import_module("effectful_helper")):
    print("VALUE:", mod.VALUE)
