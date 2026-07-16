from contextlib import nullcontext


def decorator(function):
    return function


if (IF_VALUE := "if"):
    pass


def with_default(value=(DEFAULT_VALUE := "default")):
    return value


@(DECORATOR_VALUE := decorator)
def decorated():
    return "decorated"


with (CONTEXT_MANAGER := nullcontext("context")) as CONTEXT_VALUE:
    pass

WITH_TARGET_SLOTS = [None]
with nullcontext("with-target") as WITH_TARGET_SLOTS[(WITH_TARGET_VALUE := 0)]:
    assert WITH_TARGET_SLOTS[WITH_TARGET_VALUE] == "with-target"

match "subject":
    case _ if (GUARD_VALUE := "guard"):
        pass

assert GUARD_VALUE == "guard"

TARGET_SLOTS = [None]
for TARGET_SLOTS[(FOR_TARGET_VALUE := 0)] in ("for-target",):
    assert TARGET_SLOTS[FOR_TARGET_VALUE] == "for-target"

COMPREHENSION_RESULT = [
    item for item in ("comprehension",) if (COMPREHENSION_VALUE := item)
]
LAMBDA = lambda: (LAMBDA_LOCAL := "lambda")
