import importlib

plugin = importlib.import_module("native_plugin")

print(plugin.NAME)
