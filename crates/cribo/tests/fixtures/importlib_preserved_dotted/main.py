"""Dotted preserved import_module targets initialize parents via the machinery.

Importing "pkg.sub" must run pkg/__init__.py first, exactly like Python.
"""

import importlib


def load(**options):
    return importlib.import_module("pkg.sub", **options)


print("before load")
module = load()
print("VALUE:", module.VALUE)
print("PARENT:", module.parent_flag())
