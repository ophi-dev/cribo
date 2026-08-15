OWNERS = []


def register(func):
    OWNERS.append(func.__module__)
    return func


class Entry:
    def __init__(self, value):
        self.value = value

    @register
    def describe(self):
        return f"entry:{self.value}"

    class Meta:
        def owner(self):
            return "meta"
