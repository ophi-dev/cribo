import importlib.machinery
import importlib.metadata
import json
import os
import sys


class InvalidDistributionMetadata(Exception):
    pass


def declared_name(value):
    return value.split(";", 1)[0].strip()


def import_prefixes(import_name):
    parts = import_name.split(".")
    return (".".join(parts[:depth]) for depth in range(1, len(parts) + 1))


def normalized_path(path):
    return os.path.normcase(str(path)).replace("\\", "/")


def normalized_import_name(import_name):
    return normalized_path(import_name.replace(".", "/")).replace("/", ".")


def import_relative_path(path, import_roots):
    path = normalized_path(path)
    for root in import_roots:
        root_prefix = normalized_path(root).rstrip("/") + "/"
        if path.startswith(root_prefix):
            return path[len(root_prefix) :]
    return path


def is_importable_file(path):
    if path.endswith(".py"):
        return True
    return any(path.endswith(suffix) for suffix in importlib.machinery.EXTENSION_SUFFIXES)


def file_ownership(path, import_roots=()):
    path = import_relative_path(path, import_roots)
    if not is_importable_file(path):
        return None

    if path.endswith(".py"):
        module_path = path[: -len(".py")]
        evidence = "installed file"
    else:
        suffix = next(
            suffix
            for suffix in importlib.machinery.EXTENSION_SUFFIXES
            if path.endswith(suffix)
        )
        module_path = path[: -len(suffix)]
        evidence = "installed extension"

    parts = module_path.split("/")
    if not parts or any(not part or part in {".", ".."} for part in parts):
        return None
    if parts[-1] == "__init__":
        parts.pop()
    if not parts:
        return None

    import_name = ".".join(parts)
    namespace_prefixes = [
        ".".join(parts[:depth]) for depth in range(1, len(parts))
    ]
    return import_name, evidence, namespace_prefixes


def file_score(import_name, path, import_roots=()):
    ownership = file_ownership(path, import_roots)
    if ownership is None:
        return None

    owned_import, evidence, namespace_prefixes = ownership
    import_name = normalized_import_name(import_name)
    depth = import_name.count(".") + 1
    if import_name == owned_import:
        return 4000 + depth, evidence
    if import_name in namespace_prefixes:
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


def add_index_candidate(index, import_name, distribution, score, evidence):
    candidates = index.setdefault(import_name, {})
    add_candidate(candidates, distribution, score, evidence)


def build_distribution_index(distributions, import_roots=()):
    index = {"prefix": {}, "exact": {}}
    for distribution in distributions:
        try:
            metadata = distribution.metadata
            project_name = metadata.get("Name")
            import_names = {
                declared_name(value)
                for value in metadata.get_all("Import-Name") or ()
                if declared_name(value)
            }
            namespace_names = {
                declared_name(value)
                for value in metadata.get_all("Import-Namespace") or ()
                if declared_name(value)
            }
        except Exception:
            continue
        if not project_name:
            continue

        conflicting_names = import_names & namespace_names
        if conflicting_names:
            names = ", ".join(sorted(conflicting_names))
            raise InvalidDistributionMetadata(
                f"Distribution '{project_name}' declares {names} in both "
                "Import-Name and Import-Namespace"
            )

        for import_name in import_names:
            depth = import_name.count(".") + 1
            add_index_candidate(
                index["prefix"],
                import_name,
                project_name,
                5000 + depth,
                "core metadata Import-Name",
            )

        for import_name in namespace_names:
            depth = import_name.count(".") + 1
            add_index_candidate(
                index["prefix"],
                import_name,
                project_name,
                2000 + depth,
                "core metadata Import-Namespace",
            )

        try:
            files = distribution.files or ()
        except Exception:
            files = ()
        for package_path in files:
            ownership = file_ownership(package_path, import_roots)
            if ownership is None:
                continue
            import_name, evidence, namespace_prefixes = ownership
            depth = import_name.count(".") + 1
            add_index_candidate(
                index["exact"],
                import_name,
                project_name,
                4000 + depth,
                evidence,
            )
            for namespace_prefix in namespace_prefixes:
                depth = namespace_prefix.count(".") + 1
                add_index_candidate(
                    index["exact"],
                    namespace_prefix,
                    project_name,
                    3000 + depth,
                    "installed namespace descendant",
                )

        try:
            top_level = distribution.read_text("top_level.txt") or ""
        except Exception:
            top_level = ""
        for root_import in top_level.split():
            add_index_candidate(
                index["prefix"],
                root_import,
                project_name,
                1000,
                "legacy top_level.txt",
            )

    return index


def merge_candidates(candidates, indexed_candidates):
    for candidate in indexed_candidates.values():
        add_candidate(
            candidates,
            candidate["distribution"],
            candidate["score"],
            candidate["evidence"],
        )


def distribution_candidates(import_name, distribution_index):
    candidates = {}
    for import_prefix in import_prefixes(import_name):
        merge_candidates(
            candidates,
            distribution_index["prefix"].get(import_prefix, {}),
        )
    normalized_name = normalized_import_name(import_name)
    merge_candidates(candidates, distribution_index["exact"].get(normalized_name, {}))
    return sorted(candidates.values(), key=lambda candidate: candidate["distribution"].lower())


def main():
    request = json.loads(sys.stdin.buffer.read().decode("utf-8"))
    search_paths = []
    for path in request["metadata_paths"] + sys.path:
        if path and path not in search_paths:
            search_paths.append(path)
    import_roots = sorted(
        {
            normalized_path(os.path.abspath(path)).rstrip("/")
            for path in search_paths
        },
        key=len,
        reverse=True,
    )

    distributions = list(importlib.metadata.distributions(path=search_paths))
    distribution_index = build_distribution_index(distributions, import_roots)
    resolutions = {
        import_name: distribution_candidates(import_name, distribution_index)
        for import_name in sorted(request["imports"])
    }
    response = json.dumps({"resolutions": resolutions}, sort_keys=True)
    sys.stdout.buffer.write(response.encode("utf-8"))


if __name__ == "__main__":
    try:
        main()
    except InvalidDistributionMetadata as error:
        print(error, file=sys.stderr)
        sys.exit(1)
