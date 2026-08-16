"""User code may legally rebind the generated loader's module-level name.

The finder captures the loader class at definition time (in the bundle
prelude), so a later collision must not break preserved runtime imports.
"""

import importlib

_CriboPreservedLoader = None


def load(**options):
    return importlib.import_module("collision_helper", **options)


print("VALUE:", load().VALUE)
print("shadow:", _CriboPreservedLoader)
