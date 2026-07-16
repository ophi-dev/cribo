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

match "subject":
    case _ if (GUARD_VALUE := "guard"):
        pass

COMPREHENSION_RESULT = [
    item for item in ("comprehension",) if (COMPREHENSION_VALUE := item)
]
LAMBDA = lambda: (LAMBDA_LOCAL := "lambda")
