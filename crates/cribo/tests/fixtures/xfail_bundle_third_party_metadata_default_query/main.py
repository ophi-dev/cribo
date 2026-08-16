import importlib.metadata as md
from importlib.metadata import PackageNotFoundError

try:

    def load(md=md.version("provider")):
        # The default executes at definition time in the module scope, where
        # ``md`` is the metadata alias; the parameter shadows it only here
        return md

    print(load())
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")
