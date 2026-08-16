import importlib.metadata

import provider

scripts = importlib.metadata.entry_points(group="console_scripts")
print(provider.VALUE, any(ep.name == "provider-cli" for ep in scripts))
