print("provider loaded")


def replace_with_int(_function):
    return 42


@replace_with_int
def wrapped():
    return "never seen"


CONSTANT = "alive"
