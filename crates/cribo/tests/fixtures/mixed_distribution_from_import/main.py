"""A from-imported external child must keep the installed root reachable.

`from mixed_package import _native` resolves the dotted submodule
mixed_package._native through the INSTALLED package's __path__ (simulated by
a decoy prepended to sys.path), which the bundled namespace cannot provide:
the bundled root must therefore stay behind PathFinder, not in front of it.
The decoy modules are evicted afterwards so the later bundled import behaves
identically in both runs.
"""

import os
import shutil
import sys
import tempfile

decoy_root = tempfile.mkdtemp()
try:
    decoy_pkg = os.path.join(decoy_root, "mixed_package")
    os.makedirs(decoy_pkg)
    with open(os.path.join(decoy_pkg, "__init__.py"), "w", encoding="utf-8") as handle:
        handle.write("")
    with open(os.path.join(decoy_pkg, "_native.py"), "w", encoding="utf-8") as handle:
        handle.write("KIND = 'native'\n")
    sys.path.insert(0, decoy_root)

    try:
        from mixed_package import _native

        marker = _native.KIND
    except ImportError:
        marker = "no native"
finally:
    sys.path.remove(decoy_root)
    shutil.rmtree(decoy_root, ignore_errors=True)
    for cached in [name for name in sys.modules if name.startswith("mixed_package")]:
        del sys.modules[cached]

import mixed_package

print(mixed_package.greet(), marker)
