print("provider loaded")

from shared import exported as shared_exported


def replace_with_shared(_function):
    return shared_exported


@replace_with_shared
def exported():
    return "never seen"
