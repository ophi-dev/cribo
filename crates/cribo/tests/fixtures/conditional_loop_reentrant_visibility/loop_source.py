for LOOP_VALUE in ("loop",):
    import loop_observer

    OBSERVED_LOOP_VALUE = loop_observer.observe_loop_value()
    assert OBSERVED_LOOP_VALUE == LOOP_VALUE
    del LOOP_VALUE
    CLEANED_LOOP_VALUE = loop_observer.loop_value_was_cleaned()
    assert CLEANED_LOOP_VALUE


class FailingTarget:
    def __setattr__(self, _name, _value):
        raise RuntimeError


try:
    for LOOP_PARTIAL_VALUE, FailingTarget().value in (("partial", "failure"),):
        pass
except RuntimeError:
    assert LOOP_PARTIAL_VALUE == "partial"
