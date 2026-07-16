import guarded


def mutate_and_return(result):
    guarded.BOOL_GUARD_VALUE = "mutated"
    return result


def capture_was_mutated():
    return guarded.BOOL_GUARD_VALUE == "mutated"
