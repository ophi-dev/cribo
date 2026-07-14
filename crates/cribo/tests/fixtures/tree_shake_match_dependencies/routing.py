class Kinds:
    READY = "ready"


def module_marker() -> str:
    return "ready"


match module_marker():
    case Kinds.READY:
        MODULE_STATE = "module-ready"
    case _:
        MODULE_STATE = "module-unknown"


def normalize_payload(payload: object) -> object:
    return payload


def should_handle(payload: object) -> bool:
    return isinstance(payload, dict)


def dispatch(payload: object) -> str:
    match normalize_payload(payload):
        case {"kind": Kinds.READY} if should_handle(payload):
            from formatting import format_kind

            result = format_kind(Kinds.READY)
        case _:
            result = "ignored"
    return f"{MODULE_STATE}:{result}"
