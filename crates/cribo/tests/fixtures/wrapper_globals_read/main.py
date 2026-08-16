"""globals() inside a bundled module's functions observes that module's state.

The provider is otherwise pure, but its function reads globals()["__name__"]:
inlined it would report the entry's name, so it takes the wrapper path.
"""

import provider

print("reported:", provider.reported_name())
print("registered:", provider.register_value("k", 42))
print("entry untouched:", "k" not in globals())
