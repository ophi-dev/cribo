import named_source

print(named_source.IF_VALUE)
print(named_source.DEFAULT_VALUE, named_source.with_default())
print(named_source.DECORATOR_VALUE is named_source.decorator)
print(named_source.decorated())
print(named_source.CONTEXT_VALUE)
print(named_source.GUARD_VALUE)
print(named_source.COMPREHENSION_VALUE, named_source.COMPREHENSION_RESULT)
print(hasattr(named_source, "LAMBDA_LOCAL"))
