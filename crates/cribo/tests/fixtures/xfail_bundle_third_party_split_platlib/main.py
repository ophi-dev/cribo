"""A split purelib/platlib installation aggregates policy across roots.

The Python source of foo lives in one site-packages root, while the
distribution's dist-info — whose RECORD names a native sibling — lives in the
interpreter's distinct platlib root. The bundling decision must consult the
owning root's metadata, keeping foo external as a whole instead of inlining
the pure source and omitting its requirement.
"""

import foo

print(foo.VALUE)
