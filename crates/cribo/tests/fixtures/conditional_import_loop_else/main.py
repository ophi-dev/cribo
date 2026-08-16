"""Loop ELSE suites run on the zero-iteration path too.

An import inside the loop body never executed, so the else suite's call
must keep its NameError instead of being rewritten to bundled access.
"""

for _ in []:
    from importlib import import_module
else:
    try:
        module = import_module("helper")
        marker = module.KIND
    except NameError:
        marker = "name error"

print(marker)

while False:
    from importlib import import_module as load
else:
    try:
        backend = load("helper")
        second = backend.KIND
    except NameError:
        second = "name error again"

print(second)
