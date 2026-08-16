"""inspect.getsource on an imported provider needs its installed source."""

import inspect

import sourced_pkg

print(inspect.getsource(sourced_pkg).strip())
