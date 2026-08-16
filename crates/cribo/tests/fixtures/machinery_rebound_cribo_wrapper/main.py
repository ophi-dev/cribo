"""Lazy wrapper initializers must survive a user global named `_cribo`.

The original program legally binds `_cribo` before the opaque preserved
import runs; the generated initializer's stdlib references must resolve
through definition-time captured support globals, not the clobbered name.
"""

import importlib

_cribo = None

options = {}
helper = importlib.import_module("jsonhelper", **options)
print(helper.dumps_kind(), _cribo)
