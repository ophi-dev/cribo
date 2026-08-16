"""A failed first import discards the module OBJECT, not just its content.

The failing body leaks its sys.modules entry into another module before
raising; CPython removes the failed module and allocates a DISTINCT object
for the retry, so the leaked reference must not observe the retried life.
"""

import importlib

import holder

options = {}
try:
    importlib.import_module("flaky", **options)
except RuntimeError as exc:
    print("caught:", exc)

second = importlib.import_module("flaky", **options)
print("ready:", second.VALUE)
print("distinct:", second is not holder.LEAKED[0], len(holder.LEAKED))
