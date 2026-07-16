try:
    raise RuntimeError("visible")
except RuntimeError as error:
    import exception_observer

    OBSERVED_ERROR = exception_observer.observe_error()
    del error

CLEANED_ERROR = exception_observer.error_was_cleaned()
