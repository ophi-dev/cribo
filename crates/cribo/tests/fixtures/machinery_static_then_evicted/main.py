"""A statically initialized wrapper must get a FRESH life after eviction.

The first life came from a rewritten static import (no machinery marker);
after sys.modules eviction, a runtime re-import must execute a fresh module
body, not reuse the stale initialized namespace.
"""

import importlib
import sys

import counter

print("first:", counter.count)
sys.modules.pop("counter", None)

options = {}
fresh = importlib.import_module("counter", **options)
print("fresh:", fresh.count, fresh is counter)
