import importlib.metadata as md
from importlib.metadata import PackageNotFoundError

try:

    class Provider:
        if False:
            # Never executes: Python does not create the class-local ``md``,
            # so the query below reaches the enclosing metadata alias
            md = str
        version = md.version("provider")

    print(Provider.version)
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")
