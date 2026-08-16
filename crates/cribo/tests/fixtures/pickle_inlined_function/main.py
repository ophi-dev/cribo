"""Functions from inlined (side-effect-free) modules keep a picklable identity.

pickle resolves a function through __import__(func.__module__) and getattr by
__qualname__: inlined definitions are stamped with their original module, and
the meta-path finder serves that module name with the export bound.
"""

import pickle

from provider import task

payload = pickle.dumps(task)
restored = pickle.loads(payload)
print("roundtrip:", restored("job"))
print("identity:", restored is task)
print("module:", task.__module__, task.__qualname__)
