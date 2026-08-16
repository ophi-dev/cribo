import sys

import self_pkg

print(self_pkg.SELF.VALUE)
print("registered:", "self_pkg" in sys.modules)
