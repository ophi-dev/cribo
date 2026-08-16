"""Classes from INLINED (side-effect-free) modules keep a picklable identity.

The inliner stamps __module__ on inlined classes; the bundle's meta-path
finder serves the original module name through an on-demand namespace
exposing the very same class objects.
"""

import pickle

from records import Entry

payload = pickle.dumps(Entry("ledger"))
restored = pickle.loads(payload)
print("roundtrip:", restored.name)
print("identity:", type(restored) is Entry)
print("module:", Entry.__module__)
