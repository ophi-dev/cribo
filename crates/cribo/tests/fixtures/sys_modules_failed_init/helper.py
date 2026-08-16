"""Self-registering module whose initialization fails."""

import sys

SELF = sys.modules[__name__]
raise RuntimeError("helper failed to initialize")
