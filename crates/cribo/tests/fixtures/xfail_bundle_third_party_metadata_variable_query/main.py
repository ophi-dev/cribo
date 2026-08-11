from importlib.metadata import version

import provider


def read(name):
    # The argument is unresolvable statically: any installed distribution may
    # be queried, so all of them conservatively stay observable
    return version(name)


print(read("provider"), provider.VALUE)
