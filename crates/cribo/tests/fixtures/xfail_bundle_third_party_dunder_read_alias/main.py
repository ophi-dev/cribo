"""Dunder reads through an assignment-derived module alias keep the provider external.

`alias = provider` rebinds the module object: `alias.__file__` observes the
same installed-layout global as `provider.__file__` would.
"""

from pathlib import Path

import aliased_spec_pkg

alias = aliased_spec_pkg
indirect = alias

print("data:", Path(indirect.__file__).with_name("data.txt").read_text().strip())
