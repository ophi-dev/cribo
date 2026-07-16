from mixed_package import describe

try:
    import mixed_package._native as native
except ImportError:
    native = None


print(describe(), native is None)
