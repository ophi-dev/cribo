class Status:
    READY = "first-ready"
    PENDING = "first-pending"
    MAPPING_KEY = "kind"


class Event:
    __match_args__ = ("kind",)

    def __init__(self, kind):
        self.kind = kind


LAST_RESULT = "first-unset"


def match_class(value):
    match value:
        case Event(Status.READY):
            return "first-class"
        case _:
            return "first-class-miss"


def match_mapping(value):
    match value:
        case {Status.MAPPING_KEY: Status.READY}:
            return "first-mapping"
        case _:
            return "first-mapping-miss"


def match_sequence(value):
    match value:
        case [Status.READY | Status.PENDING as state]:
            return f"first-sequence:{state}"
        case _:
            return "first-sequence-miss"


def record_result(value):
    match value:
        case Status.READY:
            global LAST_RESULT
            LAST_RESULT = "first-recorded"


def get_last_result():
    return LAST_RESULT
