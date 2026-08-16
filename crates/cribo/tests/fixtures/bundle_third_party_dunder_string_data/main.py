"""Import-global names as plain string DATA must not block bundling.

Only actual dictionary lookups (`globals()["__file__"]`) read import
globals; a set of name strings is harmless data, so the pure provider
bundles normally.
"""

import string_data_pkg

print(string_data_pkg.special_count())
