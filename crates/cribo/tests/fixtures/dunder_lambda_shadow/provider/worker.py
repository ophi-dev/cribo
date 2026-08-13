print("worker loaded from", __package__)

lambda_package = lambda __package__: __package__  # noqa: E731
module_package = lambda: __package__  # noqa: E731
