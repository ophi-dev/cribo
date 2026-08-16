"""Evicting a preserved import from sys.modules re-executes the module.

Python creates and executes a fresh module after `sys.modules.pop`; the
bundle's loader must reset the wrapper state so the body runs again instead
of returning the stale initialized namespace.
"""

import importlib
import sys


def load(**options):
    return importlib.import_module("evicted_helper", **options)


first = load()
print("first VALUE:", first.VALUE)
sys.modules.pop("evicted_helper")
second = load()
print("second VALUE:", second.VALUE)
print("distinct objects:", second is not first)
