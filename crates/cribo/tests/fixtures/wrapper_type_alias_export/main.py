import wrapped_alias

print(wrapped_alias.PublicAlias.__name__)
print(wrapped_alias.PublicAlias.__value__ == list[int])
print(wrapped_alias.EVENTS)
