from importlib.metadata import version

import provider

print(provider.VALUE, version(distribution_name="provider"))
