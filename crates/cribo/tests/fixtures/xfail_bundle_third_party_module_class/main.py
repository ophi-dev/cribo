"""Providers installing a custom module class stay external.

Assigning __class__ on the sys.modules entry requires a real ModuleType
layout, which a generated SimpleNamespace rejects with TypeError.
"""

import classy_pkg

print("computed:", classy_pkg.computed)
