import guard_observer


def get_subject():
    return "captured"


class GuardResult:
    def __bool__(self):
        return guard_observer.capture_was_mutated()


match get_subject():
    case GUARDED_VALUE if GUARDED_VALUE == "not-captured":
        raise AssertionError

match get_subject():
    case REBOUND_GUARD_VALUE if (REBOUND_GUARD_VALUE := "rebound"):
        pass

assert REBOUND_GUARD_VALUE == "rebound"

match get_subject():
    case BOOL_GUARD_VALUE if guard_observer.mutate_and_return(GuardResult()):
        BOOL_GUARD_PASSED = True

assert BOOL_GUARD_PASSED
