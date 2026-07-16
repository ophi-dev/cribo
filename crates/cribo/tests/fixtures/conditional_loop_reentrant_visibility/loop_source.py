for LOOP_VALUE in ("loop",):
    import loop_observer

    OBSERVED_LOOP_VALUE = loop_observer.observe_loop_value()
    assert OBSERVED_LOOP_VALUE == LOOP_VALUE
    del LOOP_VALUE
    CLEANED_LOOP_VALUE = loop_observer.loop_value_was_cleaned()
    assert CLEANED_LOOP_VALUE
