"""Providers importing through arbitrary loader receivers stay external.

The provider's `load(loader)` receives the REAL importlib from the consumer:
its literal target is invisible to static discovery, so the provider must
keep its installed distribution.
"""

import importlib

import loader_pkg

backend = loader_pkg.load(importlib)
print("backend:", backend.VALUE)
