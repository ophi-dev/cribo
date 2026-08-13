"""Classes exported by bundled modules keep a picklable module identity.

pickle resolves a class through __import__(cls.__module__) and getattr by
qualname: the bundle's meta-path finder serves the bundled module under its
original name, and the wrapper init stamps __module__, so the lookup yields
the very same class object.
"""

import pickle

from models import Item

payload = pickle.dumps(Item("widget"))
restored = pickle.loads(payload)
print("roundtrip:", restored.name)
print("identity:", type(restored) is Item)
print("module:", Item.__module__)
