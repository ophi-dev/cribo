print("worker loaded from", __package__)


def with_param(__package__):
    return __package__


def with_local():
    __name__ = "local"
    return __name__


def normal():
    return __package__
