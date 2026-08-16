"""A module-map constraint must keep the dependency closure external too.

The module-map entry pins mapped_pkg to `mapped-pkg>=2`, but the installed
distribution is version 1.0: the constraint cannot be satisfied by bundling,
so mapped_pkg stays external. Its declared dependency dep-pkg is then part of
an external distribution's requirement closure and must also stay external,
even though dep_pkg is pure Python and imported directly by the entry module.
"""

import mapped_pkg
import dep_pkg

print(mapped_pkg.VALUE, dep_pkg.VALUE)
