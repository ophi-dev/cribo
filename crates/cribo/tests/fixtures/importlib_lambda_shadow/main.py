"""Lambda parameters shadow enclosing import aliases, exactly like Python.

`lambda importlib: importlib.import_module("untouched_helper")` dispatches to
the ARGUMENT's import_module at runtime; bundling must neither rewrite the
call nor bundle its apparent target.
"""

import importlib


class Fake:
    @staticmethod
    def import_module(name):
        return f"fake load of {name}"


load = lambda importlib: importlib.import_module("untouched_helper")  # noqa: E731

print("lambda:", load(Fake))
print("comprehension:", [importlib.upper() for importlib in ("a", "b")])
