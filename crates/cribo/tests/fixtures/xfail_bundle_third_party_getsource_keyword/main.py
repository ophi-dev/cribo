"""Source inspection through the object= keyword keeps the provider external.

CPython accepts inspect.getsource(object=provider); the bundled namespace has
no on-disk source to serve, so the observed provider stays installed.
"""

import inspect

import keyword_sourced_pkg

print("has source:", bool(inspect.getsource(object=keyword_sourced_pkg)))
