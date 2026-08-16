"""Module-level rebinding of import globals keeps source-order semantics.

`before` observes the provider's original __name__; the rebinding then
becomes the module's visible name, without mutating the entry's globals.
"""

import provider

print("before:", provider.BEFORE)
print("after:", provider.AFTER)
print("module name:", provider.__name__)
print("entry name:", __name__)
