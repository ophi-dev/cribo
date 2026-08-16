"""Runtime imports of bundled modules must yield REAL module objects.

inspect.ismodule, isinstance against types.ModuleType, and weak references
must behave exactly like the original import.
"""

import importlib
import inspect
import types
import weakref

from models import VERSION, Token

module_name = "".join(["mod", "els"])
loaded = importlib.import_module(module_name)
print(inspect.ismodule(loaded), isinstance(loaded, types.ModuleType))
print(weakref.ref(loaded)() is loaded)
print(loaded.Token is Token, loaded.VERSION == VERSION, loaded.VERSION)
