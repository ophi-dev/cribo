import importlib


def touch():
    print("context evaluated")
    return "ignored"


# The package argument is evaluated but ignored for absolute names: the target
# is statically known, so it is bundled and the evaluation is preserved
backend = importlib.import_module("eval_pkg", package=touch())
print(backend.VALUE)
