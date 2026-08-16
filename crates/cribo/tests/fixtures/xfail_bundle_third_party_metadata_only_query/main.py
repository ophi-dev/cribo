from importlib.metadata import PackageNotFoundError, version

try:
    print(version("provider"))
except PackageNotFoundError:
    raise SystemExit("provider metadata missing")
