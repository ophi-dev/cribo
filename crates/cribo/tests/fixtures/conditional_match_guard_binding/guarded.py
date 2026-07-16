def get_subject():
    return "captured"


match get_subject():
    case GUARDED_VALUE if GUARDED_VALUE == "not-captured":
        raise AssertionError

match get_subject():
    case REBOUND_GUARD_VALUE if (REBOUND_GUARD_VALUE := "rebound"):
        pass

assert REBOUND_GUARD_VALUE == "rebound"
