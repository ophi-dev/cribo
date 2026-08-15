"""Definitions nested in compound suites and function bodies keep identity.

A method defined under `if True:` inside a class body, and an inner function
created when a bundled factory runs, must both carry the provider module's
__module__ — including at decorator time, before any outer stamp could help.
"""

import provider

entry = provider.Entry()
worker = provider.make_worker()
print("observed:", provider.OWNERS)
print("tagged:", provider.Entry.tagged.__module__, entry.tagged())
print("worker:", worker.__module__, worker())
