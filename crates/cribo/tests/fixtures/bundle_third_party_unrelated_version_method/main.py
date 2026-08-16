import importlib

import provider

# ``provider.version`` merely shares its name with the metadata query API; it
# must not force ``provider`` to stay installed
print(provider.version("provider"))

backend = importlib.import_module("provider")
print(backend.KIND)
