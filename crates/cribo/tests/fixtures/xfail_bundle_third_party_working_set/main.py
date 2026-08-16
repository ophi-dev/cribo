import provider


def _enumerate_installed():
    # Never called in this fixture: the scanner detects the enumeration
    # statically, and providers must stay observable through it
    import pkg_resources

    return [dist.project_name for dist in pkg_resources.working_set]


print(provider.VALUE)
