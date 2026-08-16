"""A regular parent without the child beats a portion WITH it.

The entry directory's PEP 420 portion carries ns_pkg/extra.py, but the
environment installs a REGULAR ns_pkg lacking extra. Python commits to the
regular package's __path__ and the dotted import fails; bundling the
portion's extra would silently resurrect a module the source program never
loads.
"""

try:
    import ns_pkg.extra

    print("loaded:", ns_pkg.extra.KIND)
except ModuleNotFoundError:
    print("missing")
