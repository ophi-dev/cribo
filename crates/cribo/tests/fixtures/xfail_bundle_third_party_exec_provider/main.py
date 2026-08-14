"""Providers publishing names through module-level exec stay external.

Inside a generated initializer the exec'd name would land in the function
frame instead of the module namespace.
"""

import exec_pkg

print("value:", exec_pkg.VALUE)
