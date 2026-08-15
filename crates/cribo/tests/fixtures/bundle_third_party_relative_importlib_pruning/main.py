"""Relative import_module targets must survive unreachable-module pruning.

late_checker's metadata query flips dropped_pkg external AFTER discovery, so
the pruning pass runs. rel_pkg reaches its backend only through
import_module(".backend", package="rel_pkg"): the recorded edge must be the
ABSOLUTE name "rel_pkg.backend", otherwise pruning discards the backend and
the bundled package cannot initialize.
"""

import rel_pkg

try:
    import dropped_pkg  # noqa: F401
except ImportError:
    pass

import late_checker  # noqa: F401

print(rel_pkg.backend_kind())
