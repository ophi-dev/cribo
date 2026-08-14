"""Providers with module-level annotations keep their __annotations__.

A real module records TOKEN and VALUE in provider.__annotations__; generated
namespaces reproduce neither, so annotated providers stay installed.
"""

import ann_pkg

print("annotated:", sorted(ann_pkg.__annotations__))
print("value:", ann_pkg.VALUE)
