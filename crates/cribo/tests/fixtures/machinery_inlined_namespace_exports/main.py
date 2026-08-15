"""Runtime imports of inlined modules must expose the COMPLETE namespace.

The finder registration for an inlined module is not limited to its stamped
classes: constants like VERSION are part of the real module namespace and
must be reachable through importlib.import_module too.
"""

import importlib

from models import Token, VERSION

module_name = "".join(["mod", "els"])
loaded = importlib.import_module(module_name)
print(loaded.VERSION == VERSION, loaded.Token is Token, loaded.VERSION)
