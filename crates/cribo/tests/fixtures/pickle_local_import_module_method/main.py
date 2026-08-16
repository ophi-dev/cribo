"""A user-defined import_module METHOD is not an import.

Loader.import_module merely shares the spelling; treating its dotted literal
argument as a real dynamic import would demote the bundled decoy_pkg behind
PathFinder, and pickle resolving Payload by identity would then observe the
same-named package planted on sys.path instead of the bundled namespace.
"""

import os
import pickle
import shutil
import sys
import tempfile

import decoy_pkg


class Loader:
    def import_module(self, name):
        return name


marker = Loader().import_module("decoy_pkg.missing")

decoy_root = tempfile.mkdtemp()
try:
    pkg_dir = os.path.join(decoy_root, "decoy_pkg")
    os.makedirs(pkg_dir)
    with open(os.path.join(pkg_dir, "__init__.py"), "w", encoding="utf-8") as handle:
        handle.write("class Payload:\n    pass\n")
    sys.path.insert(0, decoy_root)

    restored = pickle.loads(pickle.dumps(decoy_pkg.Payload("kept")))
    print(type(restored) is decoy_pkg.Payload, restored.value, marker)
finally:
    sys.path.remove(decoy_root)
    shutil.rmtree(decoy_root, ignore_errors=True)
