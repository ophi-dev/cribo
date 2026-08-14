"""Decorated functions from inlined modules keep a picklable identity.

Their identity stamps live inside a try/except guard; the finder harvest
must still register the source module for pickle's import-based resolution.
"""

import pickle

from provider import task

payload = pickle.dumps(task)
restored = pickle.loads(payload)
print("roundtrip:", restored("job"))
print("identity:", restored is task)
print("module:", task.__module__, task.__qualname__)
