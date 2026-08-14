"""Targets of unsatisfiable version edges stay external.

dist-a pins dist-b>=2 for Windows while the inspected dist-b is 1.0: the
installer must select a compatible dist-b, so the copy is never embedded.
"""

import dist_a
import dist_b

print("a:", dist_a.VALUE)
print("b:", dist_b.VALUE)
