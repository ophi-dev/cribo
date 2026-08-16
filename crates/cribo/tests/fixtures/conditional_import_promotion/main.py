"""Imports inside conditional branches must not promote past them.

`if enabled: from importlib import import_module` binds the alias only when
the branch runs; a later call must keep the original NameError semantics
when it did not. When EVERY branch establishes the same alias, later calls
are guaranteed and keep working.
"""

enabled = False

if enabled:
    from importlib import import_module

try:
    module = import_module("helper")
    marker = module.KIND
except NameError:
    marker = "name error"

print(marker)

if marker == "name error":
    from importlib import import_module as load
else:
    from importlib import import_module as load

backend = load("helper")
print(backend.KIND)
