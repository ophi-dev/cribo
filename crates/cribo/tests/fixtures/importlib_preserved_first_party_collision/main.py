"""Preserved first-party targets beat same-named installed modules.

The entry directory resolved `helper` before bundling; the bundle's local
finder keeps that precedence even when the environment also installs a
module named `helper`.
"""

import importlib


def load(**options):
    return importlib.import_module("helper", **options)


print("VALUE:", load().VALUE)
