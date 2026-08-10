import importlib


class CustomImporter:
    def import_module(self, name):
        return f"custom:{name}"


def load(importlib):
    # The parameter shadows the module-level import: this assignment must not be
    # recorded as a real import of ``external_pkg``
    result = importlib.import_module(name="external_pkg")
    return result


print(load(CustomImporter()))
print(importlib.import_module("json").dumps({"ok": True}))
