"""A preloaded sys.modules entry must win over the bundled module.

CPython's import machinery returns an existing sys.modules entry before
invoking any finder or loader, so a replacement installed AHEAD of the
import_module call must be returned instead of (re)initializing the bundled
module.
"""

import importlib
import sys
import types

replacement = types.ModuleType("provider")
replacement.VALUE = "replacement"
sys.modules["provider"] = replacement

loaded = importlib.import_module("provider")
print(loaded.VALUE, loaded is replacement)
