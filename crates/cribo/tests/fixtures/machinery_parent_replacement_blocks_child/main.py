"""A replaced parent governs submodule resolution.

After user code replaces sys.modules['pkg'] with another module whose
__path__ lacks the child, Python raises ModuleNotFoundError for pkg.child;
the bundled registration must not resurrect the child under the foreign
parent.
"""

import importlib
import sys
import types

options = {}
first = importlib.import_module("pkg.child", **options)
print("first:", first.KIND)

replacement = types.ModuleType("pkg")
replacement.__path__ = []
sys.modules["pkg"] = replacement
sys.modules.pop("pkg.child", None)

try:
    importlib.import_module("pkg.child", **options)
    print("resurrected")
except ModuleNotFoundError:
    print("blocked")
