from shared import canonical


def replace_with_shared(_function):
    return canonical


@replace_with_shared
def wrapped():
    return "never seen"
