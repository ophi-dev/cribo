import partial_source

print(partial_source.ASSIGNED)
print(partial_source.ASSIGNMENT_VISIBLE)
print(hasattr(partial_source, "DELETED"))
print(partial_source.DELETION_APPLIED)
print(partial_source.UNPACKED_FIRST)
print(partial_source.DIRECT_UNPACKING_VISIBLE)
print(partial_source.NESTED_OUTER, partial_source.NESTED_INNER)
print(partial_source.NESTED_UNPACKING_VISIBLE)
print(partial_source.STAR_FIRST, partial_source.STAR_REST)
print(partial_source.STARRED_UNPACKING_VISIBLE)
