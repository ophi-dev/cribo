"""Extras requested for an external declarer activate its dependency edges.

The module map installs native-pkg[speed], whose speed extra requires
pure-pkg: the installer will install pure-pkg alongside the external
native-pkg, so bundling pure_pkg would split module identity between the
bundled copy and the installed one — it must stay external.
"""

import pure_pkg

import native_pkg

print(native_pkg.VALUE, pure_pkg.VALUE)
