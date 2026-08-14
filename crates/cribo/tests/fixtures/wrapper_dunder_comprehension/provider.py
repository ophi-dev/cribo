print("provider loaded")

# Python evaluates the FIRST iterable in the module scope, so this
# __name__ read observes the MODULE's name; the comprehension target then
# shadows __name__ for the remaining clauses and the element expression.
values = [x for __name__ in [__name__.upper()] for x in [__name__]]
