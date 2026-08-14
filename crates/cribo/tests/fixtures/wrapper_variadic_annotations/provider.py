print("provider loaded")


def annotated(*args: __name__, **kwargs: __package__):
    return len(args) + len(kwargs)
