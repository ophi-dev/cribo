from textwrap import dedent as shared_dedent


def replace_with_shared(_function):
    return shared_dedent


@replace_with_shared
def dedent(text):
    return text
