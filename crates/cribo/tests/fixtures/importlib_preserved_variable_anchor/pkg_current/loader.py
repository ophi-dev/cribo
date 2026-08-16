import importlib

TARGET = "pkg_other"


def load_backend():
    return importlib.import_module(".backend", TARGET, **{})
