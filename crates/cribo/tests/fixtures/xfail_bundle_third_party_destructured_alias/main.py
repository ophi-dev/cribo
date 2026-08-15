"""A destructured import_module alias keeps its provider external.

`load, = (importlib.import_module,)` binds an import callable whose later
calls import discovery never resolves; bundling the provider would lose its
backend, so the provider stays installed.
"""

import destructured_alias_pkg

print(destructured_alias_pkg.backend_kind())
