import importlib

import anchor_pkg

try:
    # Python resolves this to anchor_pkg.mod.sub and raises because
    # anchor_pkg.mod is a plain module, NOT to the sibling anchor_pkg.sub
    importlib.import_module(".sub", "anchor_pkg.mod")
except ImportError:
    print("anchor is not a package")

print(anchor_pkg.VALUE)
