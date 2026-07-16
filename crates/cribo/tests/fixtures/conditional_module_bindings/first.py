from contextlib import nullcontext

if True:
    CONDITIONAL_VALUE = "first"
    ANNOTATED_VALUE: str = "annotated"
    AUGMENTED_VALUE = 1
    AUGMENTED_VALUE += 1
    UNPACKED_LEFT, *UNPACKED_MIDDLE, UNPACKED_RIGHT = ("left", "middle", "right")

    def conditional_function():
        return CONDITIONAL_VALUE

    class ConditionalClass:
        VALUE = CONDITIONAL_VALUE

for LOOP_VALUE in ("loop",):
    pass

with nullcontext(("with", "rest")) as (WITH_VALUE, *WITH_REST):
    pass

match {"value": "matched", "rest": "captured"}:
    case {"value": MATCH_VALUE, **MATCH_REST}:
        pass
