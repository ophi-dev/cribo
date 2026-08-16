"""Membership checks through sys.modules.keys() observe registered entries.

The mapping view exposes the same names the import machinery inserts.
"""

import sys

import provider

print("present:", provider.__name__ in sys.modules.keys())
