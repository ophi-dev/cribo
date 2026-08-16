"""A destructured PEP 562 hook keeps its provider external.

`__getattr__, MARKER = (_resolve, 1)` installs a module hook exactly like a
plain def; generated namespaces never invoke hooks, so the provider stays
installed.
"""

import destructured_hook_pkg

print(destructured_hook_pkg.lazy_value)
