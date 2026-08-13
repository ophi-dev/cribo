attempts = globals().get("attempts", 0) + 1
print("attempt", attempts)
raise RuntimeError(f"failed on attempt {attempts}")
