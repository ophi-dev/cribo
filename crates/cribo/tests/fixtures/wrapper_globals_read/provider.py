def reported_name():
    return globals()["__name__"]


def register_value(key, value):
    globals()[key] = value
    return globals()[key]
