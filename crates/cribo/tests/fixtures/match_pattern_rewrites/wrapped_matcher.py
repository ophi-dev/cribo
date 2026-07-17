class WrappedStatus:
    READY = "wrapped-ready"


class WrappedEvent:
    __match_args__ = ("kind",)

    def __init__(self, kind):
        self.kind = kind


EVENT_PATTERN = WrappedEvent
STATUS_PATTERN = WrappedStatus

MATCHED_EVENT = WrappedEvent("wrapped-ready")
match MATCHED_EVENT:
    case EVENT_PATTERN(STATUS_PATTERN.READY):
        MODULE_MATCH = "wrapped-module"
    case _:
        MODULE_MATCH = "wrapped-module-miss"


def match_wrapped(value):
    match value:
        case EVENT_PATTERN(STATUS_PATTERN.READY):
            return "wrapped-class"
        case _:
            return "wrapped-class-miss"


def match_wrapped_global(value):
    global EVENT_PATTERN, STATUS_PATTERN

    match value:
        case EVENT_PATTERN(STATUS_PATTERN.READY):
            return "wrapped-global"
        case _:
            return "wrapped-global-miss"


LOADED_MODULES = []
LOADED_MODULES.append("wrapped_matcher")
