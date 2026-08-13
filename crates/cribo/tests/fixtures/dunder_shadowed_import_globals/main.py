"""Local rebindings of import globals keep their own resolution.

A parameter or local named __package__/__name__ shadows the import global
inside its scope; unshadowed reads still observe the module's values.
"""

from provider.worker import normal, with_local, with_param

print("param:", with_param("argument"))
print("local:", with_local())
print("module:", normal())
