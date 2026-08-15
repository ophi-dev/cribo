"""User globals shadowing builtins must not break the bundle's import machinery.

Rebinding module-level names like `type` or `getattr` is legal Python; the
generated loader executes lazily in the bundle's global namespace, so its
builtins must be captured at definition time rather than resolved through the
(clobbered) globals when an opaque preserved import finally runs.
"""

import importlib

type = "shadowed"  # noqa: A001
id = "also shadowed"  # noqa: A001
dict = "clobbered"  # noqa: A001
getattr = None  # noqa: A001
setattr = None  # noqa: A001
BaseException = "gone"  # noqa: A001

options = {}
helper = importlib.import_module("helper", **options)
print(helper.KIND, type, id)
