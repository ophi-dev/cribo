type SharedAlias = list[SharedAlias]
type _PrivateAlias = tuple[SharedAlias]
T = int
type ShadowedAlias[T] = list[T]


def alias_is_recursive():
    return SharedAlias.__value__.__args__[0] is SharedAlias


def alias_type_parameter_is_scoped():
    parameter = ShadowedAlias.__type_params__[0]
    return ShadowedAlias.__value__.__args__[0] is parameter and T is int
