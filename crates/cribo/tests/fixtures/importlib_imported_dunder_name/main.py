"""import_module(provider.__name__) resolves statically through the import.

The call stays verbatim, but its target is known: the provider is registered
with the meta-path finder so the runtime import returns the bundled module.
"""

import importlib

import provider


def load_by_name():
    return importlib.import_module(provider.__name__)


print("VALUE:", load_by_name().VALUE)
print("same object:", load_by_name() is provider)
