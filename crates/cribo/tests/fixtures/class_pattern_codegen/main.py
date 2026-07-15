import records


class Outcome:
    __match_args__ = ("code",)

    def __init__(self, code: int, payload: object) -> None:
        self.code = code
        self.payload = payload


event = records.Envelope(
    records.Record("event", records.Text("hello"), priority=3),
    source="api",
)

print(records.summarize(event))
print(
    records.inspect_patterns(
        {
            "active": True,
            "items": [records.Text("first"), records.Text("second")],
            "source": "fixture",
        }
    )
)
print(records.inspect_patterns(records.Text("left")))
print(records.inspect_grouped_as(records.Text("right")))
print(records.inspect_nested_as(records.Text("nested")))

match Outcome(200, event):
    case Outcome(
        int(code),
        payload=records.Envelope(
            records.Record(
                "event",
                records.Text(str(text)),
                priority=int(priority),
            ),
            source=str(source),
        ),
    ):
        print(f"entry:{code}:{source}:{text}:{priority}")
    case _:
        print("entry:no-match")
