"""Providers asserting their own module identity stay external.

The provider checks isinstance(sys.modules[__name__], types.ModuleType) at
import time; a generated SimpleNamespace would fail the assertion.
"""

import self_checking_pkg

print("value:", self_checking_pkg.VALUE)
