"""A decorator returning a SHARED imported object must not be re-stamped.

The wrapper init only stamps a decorator result that still carries the
definition's __name__; stamping the shared `canonical` function would
corrupt its __module__/__qualname__ for every other reference.
"""

from provider import wrapped
from shared import canonical

print("same object:", wrapped is canonical)
print("module:", wrapped.__module__)
print("qualname:", wrapped.__qualname__)
print("call:", wrapped())
