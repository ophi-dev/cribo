def get_subject():
    return "captured"


match get_subject():
    case GUARDED_VALUE if GUARDED_VALUE == "not-captured":
        raise AssertionError
