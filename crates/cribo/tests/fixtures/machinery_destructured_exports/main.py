"""Exports created by destructuring are part of the runtime namespace.

`LEFT, RIGHT = values` binds module-scope exports exactly like plain
assignments; a computed runtime import must expose them.
"""

import importlib

from models import LEFT, RIGHT, Token

module_name = "".join(["mod", "els"])
loaded = importlib.import_module(module_name)
print(loaded.LEFT == LEFT, loaded.RIGHT == RIGHT, loaded.Token is Token, loaded.RIGHT)
