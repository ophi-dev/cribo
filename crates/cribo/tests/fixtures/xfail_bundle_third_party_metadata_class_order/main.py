import importlib.metadata as md
from importlib.metadata import PackageNotFoundError

try:

    class Provider:
        # Class namespaces bind in source order: this query uses the enclosing
        # metadata alias, which the next line rebinds
        version = md.version("provider")
        md = "rebound"

    print(Provider.version, Provider.md)
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")
