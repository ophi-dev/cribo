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


class CompoundContext:
    def __enter__(self):
        return "partial", "failure"

    def __exit__(self, _exc_type, _exc_value, _traceback):
        return False


class FailingTarget:
    def __setattr__(self, _name, _value):
        raise RuntimeError


try:
    with SuccessfulContext() as EARLY_WITH_VALUE, FailingContext():
        pass
except RuntimeError:
    assert EARLY_WITH_VALUE == "with"


try:
    with CompoundContext() as (WITH_PARTIAL_VALUE, FailingTarget().value):
        pass
except RuntimeError:
    assert WITH_PARTIAL_VALUE == "partial"
