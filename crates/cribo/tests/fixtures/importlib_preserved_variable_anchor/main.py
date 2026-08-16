"""A relative preserved call with a VARIABLE anchor names no static candidate.

`import_module(".backend", TARGET, **{})` anchors at whatever package TARGET
holds at runtime — here pkg_other, not the containing pkg_current. Anchoring
the file-path fallback there would bundle and register pkg_current.backend,
the WRONG module; the target must stay unresolved instead, and the runtime
call resolves pkg_other.backend through the machinery (bundled and registered
by its own static import).
"""

import pkg_other.backend

from pkg_current.loader import load_backend

print(load_backend().Backend.KIND, pkg_other.backend.Backend.KIND)
