"""Providers publishing names through no-argument vars() stay external.

In a real module vars() is the module namespace; inside a generated wrapper
it would be the initializer's local dictionary.
"""

import vars_pkg

print("value:", vars_pkg.VALUE)
