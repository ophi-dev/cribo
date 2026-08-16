"""Variadic parameter annotations evaluate at definition time.

Without postponed annotations, `*args`/`**kwargs` annotation expressions
run when the wrapper init defines the function, so __annotations__ must
record the ORIGINAL module's import globals.
"""

from provider import annotated

print("args:", annotated.__annotations__["args"])
print("kwargs:", repr(annotated.__annotations__["kwargs"]))
print("call:", annotated(1, 2, three=3))
