"""A failed module load removes its sys.modules NAME unconditionally.

CPython deletes the entry even when the failing body installed a
replacement; a later import must retry the body instead of observing the
stale replacement.
"""

import sys

try:
    import failing_helper
except RuntimeError as exc:
    print("caught:", exc)

print("entry present:", "failing_helper" in sys.modules)

import failing_helper

print(failing_helper.VALUE)
