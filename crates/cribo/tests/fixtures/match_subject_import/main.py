"""A match SUBJECT evaluates before any pattern capture is bound.

The import_module call in the subject still dispatches through the imported
importlib, even when a case pattern captures into the same name.
"""

import importlib

match importlib.import_module("helper"):
    case importlib:
        print(importlib.KIND)
