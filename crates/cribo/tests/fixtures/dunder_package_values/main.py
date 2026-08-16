"""__package__ observes its real import-system value in bundled modules.

A package initializer sees its own name, a submodule sees its parent, and a
top-level module sees the empty string — not the bundle entry's value.
"""

import toplevel_helper  # noqa: F401
from provider import PACKAGE_OF_INIT
from provider.worker import package_at_call_time

print("init saw:", PACKAGE_OF_INIT)
print("worker sees:", package_at_call_time())
