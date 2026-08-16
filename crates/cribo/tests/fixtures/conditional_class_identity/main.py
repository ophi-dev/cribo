"""Classes defined in compound suites keep their pickle identity.

`if True: class Entry:` records TOKEN stamps inside the suite; the finder
registration must still expose the class so pickle's
__import__(cls.__module__) resolution works in an isolated bundle.
"""

import pickle

from provider import Entry

instance = Entry("kept")
restored = pickle.loads(pickle.dumps(instance))
print(type(restored) is Entry, restored.value)
