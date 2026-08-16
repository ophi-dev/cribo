import pkg

try:
    # The local plain module ``pkg`` wins over the site-packages package of the
    # same name: Python cannot import a submodule through a non-package parent
    import pkg.sub
except ImportError:
    print("pkg is not a package")

print(pkg.VALUE)
