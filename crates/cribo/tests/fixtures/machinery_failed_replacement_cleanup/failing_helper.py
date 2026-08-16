import sys
import types

import shared_state

shared_state.ATTEMPTS[0] += 1
if shared_state.ATTEMPTS[0] == 1:
    replacement = types.ModuleType("failing_helper")
    replacement.VALUE = "replacement"
    sys.modules["failing_helper"] = replacement
    raise RuntimeError("first attempt fails")
VALUE = "ready"
