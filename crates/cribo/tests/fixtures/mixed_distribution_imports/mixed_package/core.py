try:
    from . import _native
except ImportError:
    _native = None

try:
    from ._native import missing as native_symbol
except ImportError:
    native_symbol = None


def describe():
    return f"mixed distribution {_native is None} {native_symbol is None}"
