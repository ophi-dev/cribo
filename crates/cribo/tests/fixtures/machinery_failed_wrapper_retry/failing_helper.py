import shared_state

shared_state.ATTEMPTS[0] += 1
print("helper executing", shared_state.ATTEMPTS[0])
if shared_state.ATTEMPTS[0] == 1:
    raise RuntimeError("first attempt fails")
VALUE = "ready"
