type SharedAlias = list[SharedAlias]
type _PrivateAlias = tuple[SharedAlias]


def alias_is_recursive():
    return SharedAlias.__value__.__args__[0] is SharedAlias
