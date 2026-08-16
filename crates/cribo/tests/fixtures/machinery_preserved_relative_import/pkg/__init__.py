import importlib

backend = importlib.import_module(".backend", __package__, **{})


def backend_kind():
    return backend.KIND
