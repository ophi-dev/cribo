import exception_source


def observe_error():
    return str(exception_source.error)


def error_was_cleaned():
    return not hasattr(exception_source, "error")
