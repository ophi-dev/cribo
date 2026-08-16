"""Functions exported by bundled wrapper modules keep a picklable identity.

pickle resolves a function through __import__(func.__module__) and getattr by
__qualname__: wrapper inits stamp both, and the meta-path finder serves the
bundled module under its original name.
"""

import pickle

from tasks import task

payload = pickle.dumps(task)
restored = pickle.loads(payload)
print("roundtrip:", restored("job"))
print("identity:", restored is task)
print("module:", task.__module__, task.__qualname__)
