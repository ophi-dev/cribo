import sys

try:
    import helper
except RuntimeError:
    # Python removes a module whose execution failed; the bundle must too
    print("caught:", "helper" in sys.modules)

try:
    import helper
except RuntimeError:
    # A retry must re-execute the module, not return a stale partial namespace
    print("retried and raised again")
