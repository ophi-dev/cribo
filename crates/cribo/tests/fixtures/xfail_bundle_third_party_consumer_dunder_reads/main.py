"""Consumer code reading a provider's import-spec globals keeps it external.

A bundled namespace has no faithful __file__/__spec__, so the observed
provider must keep its installed module identity.
"""

from pathlib import Path

import spec_provider_pkg

print("data:", Path(spec_provider_pkg.__file__).with_name("data.txt").read_text().strip())
print("origin set:", spec_provider_pkg.__spec__.origin is not None)
