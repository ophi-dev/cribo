"""Inlined modules must not re-stamp a decorator's SHARED return object.

Identity stamps for inlined decorated definitions only apply when the
result still carries the definition's __name__; the shared `canonical`
function keeps its own __module__/__qualname__.
"""

from provider import wrapped
from shared import canonical

print("same object:", wrapped is canonical)
print("module:", wrapped.__module__)
print("qualname:", wrapped.__qualname__)
print("call:", wrapped())
