import importlib.metadata as md
from importlib.metadata import PackageNotFoundError

try:
    # Module-level source order: this query uses the metadata alias even
    # though a later import rebinds ``md``
    VERSION = md.version("provider")
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")

import json as md

print(VERSION, md.dumps({"ok": True}))
