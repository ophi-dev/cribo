type SharedAlias = list[SharedAlias]
type _PrivateAlias = tuple[SharedAlias]
T = str
type ShadowedAlias[T] = list[T]


class Bound:
    pass


type BoundedAlias[T: Bound] = T


def alias_is_recursive():
    return SharedAlias.__value__.__args__[0] is SharedAlias


def alias_type_parameter_is_scoped():
    parameter = ShadowedAlias.__type_params__[0]
    return ShadowedAlias.__value__.__args__[0] is parameter and T is str


def alias_bound_is_renamed():
    return BoundedAlias.__type_params__[0].__bound__ is Bound
