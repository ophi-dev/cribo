"""A metadata-queried distribution keeps its dependency closure external.

The runtime version query keeps queried_pkg itself external; installing the
external queried-pkg also installs its declared dependency shared-dep, so
inlining shared_dep would split its module identity between the bundle and
the installed copy. Both imports must stay external.
"""

import importlib.metadata

import queried_pkg
import shared_dep

print(
    queried_pkg.VALUE,
    shared_dep.VALUE,
    importlib.metadata.version("queried-pkg"),
)
