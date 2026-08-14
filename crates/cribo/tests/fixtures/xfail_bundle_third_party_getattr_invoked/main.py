"""Providers invoking retrieved import callables directly stay external.

getattr(importlib, "import_module")(...) imports without any statically
discoverable target, so the provider keeps its installed distribution.
"""

import gi_pkg

backend = gi_pkg.load()
print("backend:", backend.VALUE)
