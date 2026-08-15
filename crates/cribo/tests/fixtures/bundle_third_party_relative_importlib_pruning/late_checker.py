"""Metadata query recorded AFTER dropped_pkg was discovered flips it external."""

from importlib.metadata import version


def dropped_version():
    return version("dropped-pkg")
