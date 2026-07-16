from mixed_package import describe

try:
    import mixed_package._native as native
except ImportError:
    native = None

try:
    from mixed_package import _native as native_from
except ImportError:
    native_from = None


print(describe(), native is None, native_from is None)
