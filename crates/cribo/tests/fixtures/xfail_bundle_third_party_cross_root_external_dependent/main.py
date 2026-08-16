"""External dependency edges propagate across metadata search roots.

`cross-root-blocked` lives in one site-packages root and stays external due
to __file__ access. Its pure dependency lives in a second root; that target
must stay external too, otherwise installed and bundled copies split identity.
"""

import cross_root_blocked
import cross_root_pure

print("shared:", cross_root_blocked.marker() is cross_root_pure.MARKER)
