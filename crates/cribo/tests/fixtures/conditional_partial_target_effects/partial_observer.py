import partial_source


class FailingTarget:
    def __setattr__(self, name, value):
        object.__setattr__(
            self,
            "assignment_visible",
            partial_source.ASSIGNED == value,
        )
        raise RuntimeError

    def __delattr__(self, name):
        object.__setattr__(
            self,
            "deletion_applied",
            not hasattr(partial_source, "DELETED"),
        )
        raise RuntimeError


class FailingUnpackTarget:
    def __init__(self, expected_bindings):
        object.__setattr__(self, "expected_bindings", expected_bindings)

    def __setattr__(self, name, value):
        object.__setattr__(
            self,
            "unpacking_visible",
            all(
                getattr(partial_source, binding) == expected
                for binding, expected in self.expected_bindings
            ),
        )
        raise RuntimeError
