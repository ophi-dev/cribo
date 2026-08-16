"""A function-local importlib alias still marks external submodule roots.

The dotted external child appears only inside a function body, dispatched
through a LOCAL alias (`import importlib as il`); root collection must track
scoped bindings too, or the bundled root registers in front of PathFinder
and the installed parent's __path__ can no longer serve the child.
"""

import os
import shutil
import sys
import tempfile


def probe():
    import importlib as il

    try:
        return il.import_module("mixed_package._native", **{}).KIND
    except ImportError:
        return "no native"


decoy_root = tempfile.mkdtemp()
try:
    decoy_pkg = os.path.join(decoy_root, "mixed_package")
    os.makedirs(decoy_pkg)
    with open(os.path.join(decoy_pkg, "__init__.py"), "w", encoding="utf-8") as handle:
        handle.write("")
    with open(os.path.join(decoy_pkg, "_native.py"), "w", encoding="utf-8") as handle:
        handle.write("KIND = 'native'\n")
    sys.path.insert(0, decoy_root)

    marker = probe()
finally:
    sys.path.remove(decoy_root)
    shutil.rmtree(decoy_root, ignore_errors=True)
    for cached in [name for name in sys.modules if name.startswith("mixed_package")]:
        del sys.modules[cached]

import mixed_package

print(mixed_package.greet(), marker)
