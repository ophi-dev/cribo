"""Distributions discovered by global enumeration reach requirements.

The plugin provider is never imported directly: it is located through its
entry-point group, so the isolated deployment must still install it.
"""

from importlib.metadata import entry_points

plugins = [entry for entry in entry_points(group="demo.plugins")]
if not plugins:
    raise SystemExit("plugin provider not discovered")
plugin = plugins[0].load()
print("plugin:", plugin())
