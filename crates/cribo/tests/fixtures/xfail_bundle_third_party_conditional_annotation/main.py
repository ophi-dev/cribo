"""Annotations inside module-level control flow keep providers external.

`if enabled: TOKEN: str = "..."` records TOKEN in __annotations__ exactly
like an unconditional annotation; generated namespaces reproduce neither,
so the provider stays installed.
"""

import cond_ann_pkg

print("annotated:", sorted(cond_ann_pkg.__annotations__))
print("token:", cond_ann_pkg.TOKEN)
