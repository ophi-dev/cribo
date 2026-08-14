print("provider loaded")


class Reporter:
    # Class-body assignment shadows __name__ for DIRECT class-body reads only
    __name__ = "local"
    seen_in_body = __name__

    def module_name(self):
        # Methods resolve globals from MODULE scope: class attributes never
        # shadow names inside method bodies
        return __name__

    class Inner:
        def module_name(self):
            return __name__
