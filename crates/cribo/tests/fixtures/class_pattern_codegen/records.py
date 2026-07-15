class Text:
    __match_args__ = ("value",)

    def __init__(self, value: str) -> None:
        self.value = value


class Record:
    __match_args__ = ("kind", "payload")

    def __init__(self, kind: str, payload: object, priority: int) -> None:
        self.kind = kind
        self.payload = payload
        self.priority = priority


class Envelope:
    __match_args__ = ("record",)

    def __init__(self, record: Record, source: str) -> None:
        self.record = record
        self.source = source


def summarize(value: object) -> str:
    match value:
        case Envelope(
            Record(
                "event",
                Text(str(text)),
                priority=int(priority),
            ),
            source=str(source),
        ) if priority > 0:
            return f"{source}:event:{text}:{priority}"
        case _:
            pass
    return "no-match"


def inspect_patterns(value: object) -> str:
    match value:
        case {
            "active": True,
            "items": [Text(str(first)), *remaining],
            **metadata,
        } as whole:
            return (
                f"mapping:{first}:{len(remaining)}:"
                f"{metadata['source']}:{len(whole)}"
            )
        case Text("left" | "right"):
            return "or-pattern"
        case _:
            pass
    return "other"


def inspect_grouped_as(value: object) -> str:
    match value:
        case (Text("left") as matched) | (Text("right") as matched):
            return f"grouped-as:{matched.value}"
        case _:
            pass
    return "other"
