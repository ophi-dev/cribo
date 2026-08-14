"""Providers used as dictionary keys keep their hashable module identity.

Real modules are identity-hashable; a generated SimpleNamespace raises
TypeError when hashed.
"""

import hashed_pkg

registry = {hashed_pkg: "configured"}
print("lookup:", registry[hashed_pkg])
print("hash ok:", isinstance(hash(hashed_pkg), int))
