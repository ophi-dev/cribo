"""Deleting an imported binding must not break identity imports.

Normal Python can still import records.Entry after the consumer deletes its
own `Entry` binding; the bundle captures export VALUES before entry code
runs, so pickle's `__import__(cls.__module__)` resolution keeps working.
"""

import pickle

from records import Entry

instance = Entry("kept")
del Entry
blob = pickle.dumps(instance)
restored = pickle.loads(blob)
print(type(restored).__name__, restored.value)
