OWNERS = []


def register(func):
    OWNERS.append(func.__module__)
    return func


class Entry:
    if True:

        @register
        def tagged(self):
            return "tagged"


def make_worker():
    def worker():
        return "worker"

    return worker
