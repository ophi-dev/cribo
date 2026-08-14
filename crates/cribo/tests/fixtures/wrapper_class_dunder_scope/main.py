"""Class-body vs method scoping of import globals in a wrapper module.

A class attribute named __name__ shadows the module global only for
direct class-body expressions; methods and nested-class methods resolve
__name__ from the MODULE scope.
"""

from provider import Reporter

print("body:", Reporter.seen_in_body)
print("method:", Reporter().module_name())
print("inner:", Reporter.Inner().module_name())
