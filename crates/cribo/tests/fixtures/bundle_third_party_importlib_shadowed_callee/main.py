import shadow_pkg


class CustomImporter:
    def import_module(self, name):
        return f"custom:{name}"


print(shadow_pkg.load_via(CustomImporter()))
print(shadow_pkg.real_kind())
