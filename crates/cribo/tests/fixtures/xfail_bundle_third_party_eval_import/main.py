"""Providers importing through eval'd strings stay external.

The import encoded in the string is invisible to static discovery, so the
provider keeps its installed distribution.
"""

import ev_pkg

print("value:", ev_pkg.backend.VALUE)
