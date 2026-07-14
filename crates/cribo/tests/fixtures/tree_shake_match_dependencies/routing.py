class Kinds:
    READY = "ready"


def normalize_payload(payload: object) -> object:
    return payload


def should_handle(payload: object) -> bool:
    return isinstance(payload, dict)


def format_kind(kind: str) -> str:
    return f"handled:{kind}"


def dispatch(payload: object) -> str:
    match normalize_payload(payload):
        case {"kind": Kinds.READY} if should_handle(payload):
            return format_kind(Kinds.READY)
        case _:
            return "ignored"
