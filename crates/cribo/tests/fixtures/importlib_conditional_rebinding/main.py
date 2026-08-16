"""A conditionally rebound importlib alias keeps its target importable.

The rebinding branch is not taken, so the real importlib serves the call at
runtime; the target must be bundled and served through the machinery even
though the call cannot be rewritten statically.
"""

import importlib

USE_CUSTOM = False

if USE_CUSTOM:

    class _Custom:
        @staticmethod
        def import_module(name):
            raise AssertionError(f"custom loader must not serve {name}")

    importlib = _Custom()

module = importlib.import_module("effectful_helper")
print("VALUE:", module.VALUE)
