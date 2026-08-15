"""importlib.reload must re-execute the body over the RETAINED dictionary.

The opaque **options argument keeps the import_module call verbatim, so the
module loads through the bundle's meta-path finder and registers in
sys.modules. Python reload retains the module dictionary: the counter
increments across reloads and attributes set between them survive — only an
eviction re-import may observe pristine state.
"""

import importlib

options = {}
counter = importlib.import_module("counter", **options)

print("first:", counter.count)
counter.flag = "kept"
reloaded = importlib.reload(counter)
print("reloaded:", reloaded.count, reloaded is counter, reloaded.flag)
