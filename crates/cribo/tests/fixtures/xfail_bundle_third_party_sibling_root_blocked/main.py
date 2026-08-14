"""A blocked import root taints every root of its distribution.

The distribution provides pure `duo_frontend` and __file__-reading
`duo_backend`: bundling only the frontend would split module identity
between the bundled copy and the installed distribution.
"""

import duo_backend
import duo_frontend

print("frontend:", duo_frontend.VALUE)
print("backend:", duo_backend.locate())
