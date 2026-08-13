"""Exact module-type identity checks keep the provider a real module.

type(provider) is types.ModuleType and provider.__class__ comparisons fail on
generated namespaces, so the observed provider stays installed.
"""

import types

import identity_checked_pkg

print("exact type:", type(identity_checked_pkg) is types.ModuleType)
print("class match:", identity_checked_pkg.__class__ is types.ModuleType)
