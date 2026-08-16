"""The first comprehension iterable evaluates in the enclosing scope.

The import call inside it uses the imported importlib even though the
comprehension target rebinds the same name for the rest of the expression.
"""

import importlib

values = [importlib.VALUE for importlib in [importlib.import_module("effectful_helper")]]
print("value:", values[0])
