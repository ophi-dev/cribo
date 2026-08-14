def identity(func):
    return func


@identity
def task(name):
    return f"ran {name}"
