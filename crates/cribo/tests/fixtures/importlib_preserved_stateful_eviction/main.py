"""Eviction re-imports observe a FRESH namespace, not stale globals.

CPython creates a new module object after sys.modules.pop: a stateful
counter seeded from globals() must restart at 1 on the second life.
"""

import importlib
import sys


def load(**options):
    return importlib.import_module("stateful_helper", **options)


first = load()
print("first attempts:", first.attempts)
sys.modules.pop("stateful_helper")
second = load()
print("second attempts:", second.attempts)
