"""Replacement decorators returning immutable objects survive bundling.

Identity stamps on decorated definitions are guarded: a decorator returning
an int must not crash the wrapper init with AttributeError.
"""

from provider import CONSTANT, wrapped

print("wrapped:", wrapped)
print("constant:", CONSTANT)
