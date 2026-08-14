"""Inlined modules must not re-stamp a decorator's SHARED return object.

Identity stamps for inlined decorated definitions only apply when the
result still carries the definition's __name__; the shared `canonical`
function keeps its own __module__/__qualname__.
"""

from provider import exported
from shared import canonical

print("same object:", exported is canonical)
print("module:", exported.__module__)
print("qualname:", exported.__qualname__)
print("call:", exported())
