module_state = "global"


def local_assignment():
    return "dead-local-assignment"


def parameter():
    return "dead-parameter"


def comprehension_target():
    return "dead-comprehension-target"


def exception_target():
    return "dead-exception-target"


def pattern_capture():
    return "dead-pattern-capture"


def loop_target():
    return "dead-loop-target"


def nested_local():
    return "dead-nested-local"


def exercise(parameter: str) -> str:
    global module_state

    local_assignment = "local"
    comprehension_values = [
        comprehension_target for comprehension_target in ("comprehension",)
    ]

    try:
        raise ValueError("exception")
    except ValueError as exception_target:
        exception_value = str(exception_target)

    match {"value": "pattern"}:
        case {"value": pattern_capture}:
            pattern_value = pattern_capture

    for loop_target in ("loop",):
        loop_value = loop_target

    from math import prod as math_prod

    def nested() -> str:
        from scoped_imports import nested_value

        nested_local = "nested"
        return f"{nested_local}-{nested_value()}"

    class LocalType:
        @staticmethod
        def value() -> str:
            from scoped_imports import method_value

            return method_value()

    module_state += "-dependency"
    values = [
        parameter,
        local_assignment,
        *comprehension_values,
        exception_value,
        pattern_value,
        loop_value,
        str(math_prod((2, 3))),
        nested(),
        LocalType.value(),
        module_state,
    ]
    return "|".join(values)
