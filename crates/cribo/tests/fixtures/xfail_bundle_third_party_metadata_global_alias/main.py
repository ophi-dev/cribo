import importlib.metadata as md
from importlib.metadata import PackageNotFoundError


def refresh():
    # ``global md`` means the assignment below binds the module scope: the
    # query still resolves through the module-level metadata alias
    global md
    try:
        result = md.version("provider")
    except PackageNotFoundError:
        raise SystemExit("provider metadata missing")
    md = None
    return result


print(refresh())
