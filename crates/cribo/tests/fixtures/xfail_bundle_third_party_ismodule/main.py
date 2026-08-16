"""Providers observed through inspect.ismodule keep their real module identity.

A generated SimpleNamespace fails the type test, so the observed provider
must stay an installed, real module object.
"""

import inspect

import identity_pkg

print("is module:", inspect.ismodule(identity_pkg))
