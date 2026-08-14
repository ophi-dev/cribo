"""Comprehension scoping of import globals inside a wrapper module.

The first iterable evaluates in module scope (reads the module's
__name__); the comprehension target then shadows __name__ for later
clauses, which must NOT be rewritten to the static module name.
"""

from provider import values

print("values:", values)
