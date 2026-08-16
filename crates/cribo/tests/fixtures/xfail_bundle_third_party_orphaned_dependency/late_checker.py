"""First-party module querying the parent's metadata after it was discovered."""

from importlib.metadata import version


def check():
    return version("parent-pkg")
