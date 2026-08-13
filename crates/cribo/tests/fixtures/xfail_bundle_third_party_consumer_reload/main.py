"""Application code reloading an imported provider: the target must stay a
real registered module, so it is kept external with its installed spec."""

import importlib

import reload_target_pkg

reloaded = importlib.reload(reload_target_pkg)
print(reloaded.VALUE)
