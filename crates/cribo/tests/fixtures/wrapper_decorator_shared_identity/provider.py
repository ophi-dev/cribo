print("provider loaded")

from shared import canonical


def replace_with_shared(_function):
    return canonical


@replace_with_shared
def exported():
    return "never seen"
