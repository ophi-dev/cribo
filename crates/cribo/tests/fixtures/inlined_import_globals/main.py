"""Functions in inlined modules observe their own module's import globals."""

from provider.worker import module_doc, module_name, package_name

print("package:", package_name())
print("name:", module_name())
print("doc:", module_doc())
