import importlib.machinery
import importlib.metadata
import json
import sys


def declared_name(value):
    return value.split(";", 1)[0].strip()


def is_import_prefix(prefix, import_name):
    return import_name == prefix or import_name.startswith(prefix + ".")


def normalized_path(path):
    return str(path).replace("\\", "/")


def is_importable_file(path):
    if path.endswith(".py"):
        return True
    return any(path.endswith(suffix) for suffix in importlib.machinery.EXTENSION_SUFFIXES)


def file_score(import_name, path):
    if not is_importable_file(path):
        return None

    prefix = import_name.replace(".", "/")
    depth = import_name.count(".") + 1
    if path == prefix + ".py" or path == prefix + "/__init__.py":
        return 4000 + depth, "installed file"

    for suffix in importlib.machinery.EXTENSION_SUFFIXES:
        if path == prefix + suffix or path == prefix + "/__init__" + suffix:
            return 4000 + depth, "installed extension"

    if path.startswith(prefix + "/"):
        return 3000 + depth, "installed namespace descendant"

    return None


def add_candidate(candidates, distribution, score, evidence):
    current = candidates.get(distribution)
    if current is None or score > current["score"]:
        candidates[distribution] = {
            "distribution": distribution,
            "score": score,
            "evidence": evidence,
        }


def distribution_candidates(import_name, distributions):
    candidates = {}
    for distribution in distributions:
        project_name = distribution.metadata.get("Name")
        if not project_name:
            continue

        for value in distribution.metadata.get_all("Import-Name") or ():
            import_prefix = declared_name(value)
            if import_prefix and is_import_prefix(import_prefix, import_name):
                depth = import_prefix.count(".") + 1
                add_candidate(
                    candidates,
                    project_name,
                    5000 + depth,
                    "core metadata Import-Name",
                )

        for value in distribution.metadata.get_all("Import-Namespace") or ():
            import_prefix = declared_name(value)
            if import_prefix and is_import_prefix(import_prefix, import_name):
                depth = import_prefix.count(".") + 1
                add_candidate(
                    candidates,
                    project_name,
                    2000 + depth,
                    "core metadata Import-Namespace",
                )

        try:
            files = distribution.files or ()
        except (OSError, ValueError):
            files = ()
        for package_path in files:
            evidence = file_score(import_name, normalized_path(package_path))
            if evidence is not None:
                score, description = evidence
                add_candidate(candidates, project_name, score, description)

        try:
            top_level = distribution.read_text("top_level.txt") or ""
        except (OSError, ValueError):
            top_level = ""
        root_import = import_name.split(".", 1)[0]
        if root_import in top_level.split():
            add_candidate(candidates, project_name, 1000, "legacy top_level.txt")

    return sorted(candidates.values(), key=lambda candidate: candidate["distribution"].lower())


def main():
    request = json.load(sys.stdin)
    search_paths = []
    for path in request["metadata_paths"] + sys.path:
        if path and path not in search_paths:
            search_paths.append(path)

    distributions = list(importlib.metadata.distributions(path=search_paths))
    resolutions = {
        import_name: distribution_candidates(import_name, distributions)
        for import_name in sorted(request["imports"])
    }
    json.dump({"resolutions": resolutions}, sys.stdout, sort_keys=True)


if __name__ == "__main__":
    main()
