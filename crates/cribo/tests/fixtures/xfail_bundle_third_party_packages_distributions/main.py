import importlib.metadata

import provider

mapping = importlib.metadata.packages_distributions()
print(provider.VALUE, "provider" in mapping)
