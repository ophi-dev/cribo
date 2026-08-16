"""A failed module execution must be retried, not observed half-done.

Python discards a module whose body raised; a later import re-executes it.
Wrapper initializers must reset their guards on failure so the retry runs
the body again instead of returning the stale partial namespace.
"""

try:
    import failing_helper
except RuntimeError as exc:
    print("caught:", exc)

import failing_helper

print(failing_helper.VALUE)
