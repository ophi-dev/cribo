"""sys.modules observations through assignment-derived aliases register targets.

`loaded = sys.modules` rebinds the mapping: the lookup observes the same
entries that a normal Python import inserts.
"""

import sys

loaded = sys.modules

import provider

print("registered:", loaded[provider.__name__] is provider)
