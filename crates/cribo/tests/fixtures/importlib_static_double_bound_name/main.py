import importlib

try:
    importlib.import_module("json", name="other")
except TypeError:
    print("caught: name bound twice")
