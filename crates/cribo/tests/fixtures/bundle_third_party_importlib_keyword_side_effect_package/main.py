import importlib

import side_pkg


def touch():
    raise RuntimeError("package context evaluated")


try:
    importlib.import_module(name="side_pkg", package=touch())
except RuntimeError:
    print("package context evaluated")

print(side_pkg.VALUE)
