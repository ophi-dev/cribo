"""Module deliberately replacing itself in sys.modules."""

import sys
import types

replacement = types.SimpleNamespace(KIND="replacement")
sys.modules[__name__] = replacement
