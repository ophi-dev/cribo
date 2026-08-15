"""Unpickling by original module name must yield the BUNDLED class identity.

pickle resolves a class through __import__(cls.__module__): the bundled
namespace registered under the original name must win over a same-named
installed distribution (simulated by a decoy package prepended to sys.path
AFTER the bundled import ran), because only the bundled namespace holds the
very same class object the pickled instance was created from.
"""

import os
import pickle
import shutil
import sys
import tempfile

import pickled_pkg

decoy_root = tempfile.mkdtemp()
try:
    decoy_pkg = os.path.join(decoy_root, "pickled_pkg")
    os.makedirs(decoy_pkg)
    with open(os.path.join(decoy_pkg, "__init__.py"), "w", encoding="utf-8") as handle:
        handle.write("class Token:\n    marker = 'decoy'\n")
    sys.path.insert(0, decoy_root)

    token = pickle.loads(pickle.dumps(pickled_pkg.Token("round-trip")))
    print(type(token) is pickled_pkg.Token, token.marker, token.value)
finally:
    sys.path.remove(decoy_root)
    shutil.rmtree(decoy_root, ignore_errors=True)
