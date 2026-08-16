"""Lambda parameters named like import globals keep their own resolution."""

from provider.worker import lambda_package, module_package

print("lambda:", lambda_package("argument"))
print("module:", module_package())
