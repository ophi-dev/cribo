import importlib


def load_later(**options):
    # Preserved call: the target initializes at THIS call site, not at bundle
    # startup, keeping Python's lazy import semantics
    return importlib.import_module("lazy_helper", **options)


def never_called(**options):
    # An untaken call must not execute its target at all
    return importlib.import_module("untouched_helper", **options)


print("before any import")
print(load_later().VALUE)
