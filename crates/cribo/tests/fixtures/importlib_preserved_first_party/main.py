import importlib

options = {}
# Opaque arguments preserve the call verbatim; the first-party target must be
# bundled, registered in sys.modules, and eagerly initialized so the runtime
# call resolves it inside the single-file bundle
helper = importlib.import_module("helper", **options)
print(helper.VALUE)
