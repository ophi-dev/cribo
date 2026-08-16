"""A decorator returning a SAME-NAMED imported callable must not be re-stamped.

The stdlib textwrap.dedent shares the decorated binding's __name__, so a
name-only identity check would wrongly treat it as the newly defined object
and corrupt the stdlib function's attribution for the entire process; the
provenance probe (creation module) must reject it.
"""

import textwrap

from provider import dedent

print("same object:", dedent is textwrap.dedent)
print("module:", dedent.__module__)
print("call:", dedent("    indented").strip())
