"""Dependencies of external distributions stay external.

`native-dep` ships a native artifact and stays installed; its Requires-Dist
edge on `pure-dep` makes the installer provide pure_dep transitively.
Bundling pure_dep would split module identity: native_dep would import the
installed copy while bundled code used the inlined one.
"""

import native_dep
import pure_dep

print("shared:", native_dep.uses() is pure_dep.MARKER)
