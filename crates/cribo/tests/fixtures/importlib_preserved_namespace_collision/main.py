"""Rebinding a global that matches a bundled module name cannot break imports.

The finder captures namespace OBJECTS where they are created, so a user
global assigned before the preserved import does not replace the module.
"""

import importlib

globals()["collision_target"] = "sentinel"


def load(**options):
    return importlib.import_module("collision_target", **options)


print("VALUE:", load().VALUE)
print("global:", globals()["collision_target"])
