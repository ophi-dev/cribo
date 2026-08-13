"""Providers checked against types.ModuleType keep their real module identity.

A generated SimpleNamespace fails the isinstance test that the installed
module passes, so the checked provider must stay external.
"""

import types

import typed_pkg

print("is module:", isinstance(typed_pkg, types.ModuleType))
