"""A decorator returning a SAME-NAMED imported callable must not be re-stamped.

The shared function's __name__ equals the decorated binding, so a name-only
identity check would wrongly treat it as the newly defined object; the
provenance probe (creation module) must reject it inside wrapper inits too,
keeping the shared callable attributed to its defining module.
"""

import shared
from provider import exported

print("same object:", exported is shared.exported)
print("module:", exported.__module__)
print("qualname:", exported.__qualname__)
print("call:", exported())
