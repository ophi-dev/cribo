import importlib.metadata as md
from importlib.metadata import PackageNotFoundError


def read():
    # Called below while the metadata alias is still active, BEFORE the later
    # rebinding import: both module views apply to function-body lookups
    try:
        return md.version("provider")
    except PackageNotFoundError:
        raise SystemExit("provider metadata missing")


print(read())

import json as md

print(md.dumps({"ok": True}))
