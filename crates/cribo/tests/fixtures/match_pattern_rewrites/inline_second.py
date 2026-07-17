class Status:
    READY = "second-ready"
    PENDING = "second-pending"
    MAPPING_KEY = "kind"


class Event:
    __match_args__ = ("kind",)

    def __init__(self, kind):
        self.kind = kind


LAST_RESULT = "second-unset"


def match_class(value):
    match value:
        case Event(Status.READY):
            return "second-class"
        case _:
            return "second-class-miss"


def match_mapping(value):
    match value:
        case {Status.MAPPING_KEY: Status.READY}:
            return "second-mapping"
        case _:
            return "second-mapping-miss"


def match_sequence(value):
    match value:
        case [Status.READY | Status.PENDING as state]:
            return f"second-sequence:{state}"
        case _:
            return "second-sequence-miss"


def record_result(value):
    match value:
        case Status.READY:
            global LAST_RESULT
            LAST_RESULT = "second-recorded"


def get_last_result():
    return LAST_RESULT
