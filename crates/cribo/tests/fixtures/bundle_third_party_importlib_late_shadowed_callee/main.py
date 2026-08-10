import late_shadow_pkg

try:
    late_shadow_pkg.broken_loader()
except UnboundLocalError:
    print("UnboundLocalError preserved")

print(late_shadow_pkg.real_kind())
