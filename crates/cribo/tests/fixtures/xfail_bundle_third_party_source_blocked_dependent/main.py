"""Every dependency of an external distribution stays external.

`blocked-dep` reads __file__ (a source blocker) and stays installed; its
Requires-Dist chain (mid-pure -> leaf-pure) is installed transitively, so
bundling leaf_pure would split module identity between the inlined copy
and the installed copy blocked_dep's code imports.
"""

import blocked_dep
import leaf_pure

print("shared:", blocked_dep.uses() is leaf_pure.MARKER)
