"""Module-dictionary introspection keeps providers external.

`provider.__dict__` (and `vars(provider)`) expose the COMPLETE module
dictionary including private bindings, which generated namespaces do not
reproduce; such providers stay installed.
"""

import dict_pkg

print(dict_pkg.__dict__["_TOKEN"])
print("PUBLIC" in vars(dict_pkg))
