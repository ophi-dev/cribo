"""A walrus-bound PEP 562 hook keeps its provider external.

`(__getattr__ := _resolve)` installs a module hook exactly like a def;
generated namespaces never invoke hooks, so the provider stays installed.
"""

import walrus_hook_pkg

print(walrus_hook_pkg.lazy_value)
