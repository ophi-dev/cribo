"""Preserved first-party targets beat same-named installed modules.

The entry directory resolved `helper` and `helperpkg` before bundling; the
bundle's local finder keeps that precedence — for plain modules AND
packages — even when the environment also installs modules with the same
names.
"""

import importlib


def load_module(**options):
    return importlib.import_module("helper", **options)


def load_package(**options):
    return importlib.import_module("helperpkg", **options)


print("VALUE:", load_module().VALUE)
print("PKG VALUE:", load_package().VALUE)
