"""Providers replacing module-level __builtins__ stay external.

A module-level `__builtins__` mapping becomes the builtins namespace of
every function defined in the real module; the generated wrapper init
cannot reproduce that capture, so such providers keep their installed
identity.
"""

import builtins_pkg

print("measure:", builtins_pkg.measure([]))
