"""Providers that rebind `globals` stay external.

Wrapper finalization redirects zero-argument `globals()` calls to the
generated module namespace without checking bindings; a provider-defined
`globals` callable would silently be replaced, so such providers keep
their installed identity.
"""

import globals_pkg

print("value:", globals_pkg.VALUE)
