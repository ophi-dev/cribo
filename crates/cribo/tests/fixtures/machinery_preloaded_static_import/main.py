"""A preloaded sys.modules entry must win over a STATIC bundled import.

CPython's import statement returns an existing sys.modules entry before any
loading; a replacement installed ahead of `import provider` must be bound
instead of (re)initializing the bundled module.
"""

import sys
import types

replacement = types.ModuleType("provider")
replacement.VALUE = "replacement"
sys.modules["provider"] = replacement

import provider

print(provider.VALUE, provider is replacement)
