"""Preserved RELATIVE importlib calls must have their targets bundled.

`import_module(".backend", __package__, **{})` stays verbatim (opaque
argument shape), but its statically resolvable target must be discovered
and registered with the finder, or an isolated deployment fails.
"""

import pkg

print(pkg.backend_kind())
