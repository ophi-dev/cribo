from first import alias_type_parameter_is_scoped as first_alias_scope
from first import alias_is_recursive as first_alias_is_recursive
from second import alias_type_parameter_is_scoped as second_alias_scope
from second import alias_is_recursive as second_alias_is_recursive

print(first_alias_is_recursive())
print(second_alias_is_recursive())
print(first_alias_scope())
print(second_alias_scope())
