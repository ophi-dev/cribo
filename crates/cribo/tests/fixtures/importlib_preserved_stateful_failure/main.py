"""A failed preserved import discards partial namespace mutations.

Python removes a failed module and retries with a FRESH namespace, so a
module counting its own attempts in globals() observes attempt 1 both times.
"""

import importlib


def load(**options):
    return importlib.import_module("stateful_flaky", **options)


for round_number in (1, 2):
    try:
        load()
    except RuntimeError as error:
        print(f"round {round_number}: {error}")
