import loop_source


def observe_loop_value():
    return loop_source.LOOP_VALUE


def loop_value_was_cleaned():
    return not hasattr(loop_source, "LOOP_VALUE")
