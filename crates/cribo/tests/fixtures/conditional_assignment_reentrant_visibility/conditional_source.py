if __name__ != "__main__":
    VALUE = "early"
    import conditional_observer

    OBSERVED_VALUE = conditional_observer.observe_value()
    assert VALUE == OBSERVED_VALUE
    from math import pi as VALUE

    assert OBSERVED_VALUE == "early"
