"""Metadata queries through a dynamically imported metadata module keep owners external.

importlib.import_module("importlib.metadata") returns the metadata module
itself; version() calls through the binding need the provider's dist-info.
"""

import importlib

import versioned_pkg

metadata = importlib.import_module("importlib.metadata")
print("version:", metadata.version("versioned-pkg"))
print("value:", versioned_pkg.VALUE)
