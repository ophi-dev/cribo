"""Preserved import_module calls keep Python's argument evaluation order.

An invalid keyword must raise TypeError from import_module itself BEFORE the
bundled target module executes any side effects.
"""

import importlib


def load(**options):
    return importlib.import_module("effectful_helper", **options)


try:
    load(bogus_option=True)
except TypeError:
    print("TypeError raised before target import")

module = load()
print("VALUE:", module.VALUE)
