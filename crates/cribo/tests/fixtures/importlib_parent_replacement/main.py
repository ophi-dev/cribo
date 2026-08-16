"""A preloaded PARENT package entry must control dotted import resolution.

Python resolves import_module("dotted_pkg.sub") through the parent's
__path__ when sys.modules["dotted_pkg"] is preloaded (and no child entry
exists), so a replacement parent selects the replacement implementation
instead of the bundled child.
"""

import importlib
import os
import shutil
import sys
import tempfile
import types

import dotted_pkg

print(dotted_pkg.BANNER)

root = tempfile.mkdtemp()
try:
    impl_dir = os.path.join(root, "impl")
    os.makedirs(impl_dir)
    with open(os.path.join(impl_dir, "sub.py"), "w", encoding="utf-8") as handle:
        handle.write("KIND = 'replacement'\n")
    replacement = types.ModuleType("dotted_pkg")
    replacement.__path__ = [impl_dir]
    sys.modules["dotted_pkg"] = replacement

    loaded = importlib.import_module("dotted_pkg.sub")
    print(loaded.KIND)
finally:
    del sys.modules["dotted_pkg"]
    sys.modules.pop("dotted_pkg.sub", None)
    shutil.rmtree(root, ignore_errors=True)
