"""First-party module querying the provider's metadata after it was discovered."""

from importlib.metadata import version


def check():
    return version("provider")
