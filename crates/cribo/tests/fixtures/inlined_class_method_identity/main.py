"""Definitions inside bundled class bodies carry the provider's identity.

Methods (and nested classes) created while a class body executes read the
creating frame's __name__ for their __module__; both decorator-time
observation (register sees func.__module__ during class creation) and later
introspection must see the provider module, not the bundle entry's.
"""

import records

entry = records.Entry("x")
print("init module:", records.Entry.__init__.__module__)
print("meta method:", records.Entry.Meta.owner.__module__)
print("observed:", records.OWNERS)
print(entry.describe())
