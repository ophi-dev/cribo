"""Providers passed to inspect.getmodule keep their real module identity.

inspect.getmodule returns None for a generated SimpleNamespace, so the
observed provider must stay an installed, real module object.
"""

import inspect

import getmodule_pkg

module = inspect.getmodule(getmodule_pkg)
print("resolved:", module is getmodule_pkg)
