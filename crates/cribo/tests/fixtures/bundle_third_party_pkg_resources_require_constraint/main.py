def _declare_runtime_dependency():
    # Never called in this fixture: the scanner records the constrained
    # requirement statically, and it must survive into requirements.txt
    import pkg_resources

    pkg_resources.require("provider[speed]>=0.5")


print("constraint declared")
