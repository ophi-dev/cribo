"""Wrapper modules keep their docstrings observable through __doc__."""

import documented
import undocumented

print("doc:", documented.__doc__)
print("self view:", documented.SELF_DOC)
print("missing doc:", undocumented.__doc__)
