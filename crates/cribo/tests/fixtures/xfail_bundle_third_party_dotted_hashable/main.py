"""Dotted submodules bound by import-as keep hashable module identity.

`import dh_pkg.sub as sub` binds a real module object; using it as a
dictionary key requires that identity.
"""

import dh_pkg.sub as sub

registry = {sub: "configured"}
print("lookup:", registry[sub])
