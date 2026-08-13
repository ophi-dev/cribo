"""Consumers can observe a bundled dependency's sys.modules entry.

Static imports invoke the generated initializer directly, so observed targets
must register in sys.modules themselves — under their original name, holding
the very same module object.
"""

import sys

import dep

print("by dynamic key:", sys.modules[dep.__name__] is dep)
print("by literal key:", "dep" in sys.modules)
print("value:", dep.VALUE)
