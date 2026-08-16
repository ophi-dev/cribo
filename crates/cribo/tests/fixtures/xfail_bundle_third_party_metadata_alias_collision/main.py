import importlib.metadata as md
from importlib.metadata import PackageNotFoundError


def unrelated():
    # This function-local rebinding of ``md`` must not hide the module-level
    # metadata query below from requirement collection
    import json as md

    return md.dumps({"ok": True})


try:
    print(md.version("provider"), unrelated())
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")
