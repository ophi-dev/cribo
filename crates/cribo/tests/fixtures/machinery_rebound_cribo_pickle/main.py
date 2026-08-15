"""Pickling must survive a user global named `_cribo`.

The original program legally uses a variable named `_cribo`; the generated
lazy loader must not resolve its namespace constructor through that global
when pickle imports the inlined module by its original name.
"""

import pickle

from models import Token

dumps = pickle.dumps
loads = pickle.loads

_cribo = "user data"

token = loads(dumps(Token("round-trip")))
print(type(token) is Token, token.value, _cribo)
