"""A failing preserved import raises on EVERY attempt, exactly like Python.

Python removes a failed module from sys.modules, so a retried import
re-executes the body and fails again; the bundle's loader must reset the
wrapper state on failure instead of returning a stale partial namespace.
"""

import importlib


def load(**options):
    return importlib.import_module("flaky_helper", **options)


for attempt in (1, 2):
    try:
        load()
    except RuntimeError as error:
        print(f"attempt {attempt}: {error}")
