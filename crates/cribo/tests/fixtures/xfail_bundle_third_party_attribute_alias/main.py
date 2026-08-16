"""An attribute-held import callable keeps its provider external.

`Loader.load = staticmethod(importlib.import_module)` invoked with a target
import discovery never resolves would lose the backend if bundled; the
provider stays installed.
"""

import attr_alias_pkg

print(attr_alias_pkg.backend_kind())
