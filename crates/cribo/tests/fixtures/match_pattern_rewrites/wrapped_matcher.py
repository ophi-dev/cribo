import json

assert json.__name__ == "json"


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


value = "module-value"
if value:

    def match_capture(subject):
        match subject:
            case [value] if value == "captured-value":
                return value
        return "capture-miss"


def match_import_capture(subject):
    match subject:
        case [json]:
            return json
    return "import-capture-miss"


def match_import_capture_scope(subject):
    try:
        match subject:
            case json.JSONEncoder():
                return "import-module-match"
            case [json]:
                return json
    except UnboundLocalError:
        return "capture-scope-local"
    return "import-scope-miss"


global_capture = "global-before"


def match_global_capture(subject):
    if subject is None:
        global global_capture

    match subject:
        case [global_capture] if global_capture == "captured-global":
            return global_capture
    return "global-capture-miss"


def get_global_capture():
    return global_capture


LOADED_MODULES = []
LOADED_MODULES.append("wrapped_matcher")
