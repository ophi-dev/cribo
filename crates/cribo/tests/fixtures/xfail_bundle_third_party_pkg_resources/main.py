"""Legacy pkg_resources readers keep their anchor packages external.

resource_filename resolves the package through the import system and its
installed layout, so the anchor must stay installed rather than bundled.
"""

from pathlib import Path

import pkg_resources

print(
    "data:",
    Path(pkg_resources.resource_filename("res_pkg", "data.txt")).read_text().strip(),
)
