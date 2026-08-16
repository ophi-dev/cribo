"""Bundled application code reading an imported provider's package data."""

import importlib.resources

import res_provider_pkg

print("provider value:", res_provider_pkg.VALUE)
print(
    "data:",
    importlib.resources.files("res_provider_pkg").joinpath("data.txt").read_text().strip(),
)
