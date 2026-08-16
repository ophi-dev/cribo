import sys

import holder

holder.LEAKED.append(sys.modules["flaky"])
if len(holder.LEAKED) == 1:
    raise RuntimeError("first attempt fails")
VALUE = "ready"
