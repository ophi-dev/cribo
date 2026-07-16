class SuccessfulContext:
    def __enter__(self):
        return "with"

    def __exit__(self, _exc_type, _exc_value, _traceback):
        return False


class FailingContext:
    def __enter__(self):
        raise RuntimeError

    def __exit__(self, _exc_type, _exc_value, _traceback):
        return False


try:
    with SuccessfulContext() as EARLY_WITH_VALUE, FailingContext():
        pass
except RuntimeError:
    pass
