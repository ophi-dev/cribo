print("pkg.sub initialized")

VALUE = "sub value"


def parent_flag():
    import pkg

    return pkg.PACKAGE_FLAG
