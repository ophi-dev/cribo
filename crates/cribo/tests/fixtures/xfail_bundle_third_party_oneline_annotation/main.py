"""Annotations in one-line suites keep providers external.

`if enabled: TOKEN: str = "..."` records TOKEN in __annotations__ exactly
like the indented form; generated namespaces reproduce neither, so the
provider stays installed.
"""

import oneline_ann_pkg

print("annotated:", sorted(oneline_ann_pkg.__annotations__))
print("token:", oneline_ann_pkg.TOKEN)
