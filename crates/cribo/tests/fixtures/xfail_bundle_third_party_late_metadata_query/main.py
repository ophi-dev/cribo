import importlib

provider = importlib.import_module("provider")

import late_checker

print(late_checker.check(), provider.VALUE)
