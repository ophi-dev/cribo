//! Meta-path finder serving bundled modules to REAL runtime imports.
//!
//! Preserved calls (opaque arguments such as `**options`) stay verbatim in the
//! bundle and execute as REAL runtime imports, and external consumers such as
//! `pickle` resolve classes through `__import__(cls.__module__)`. Bundled
//! modules are made reachable through Python's own import machinery: a
//! `sys.meta_path` finder maps their original names to the bundled init
//! functions and namespace objects, so runtime imports keep exact Python
//! semantics — arguments are evaluated and validated by `import_module`
//! itself, parent packages are initialized by the machinery in order,
//! `sys.modules` is populated (and cleaned up on failure) by the machinery,
//! and nothing executes until the import actually runs.
//!
//! The finder is APPENDED to `sys.meta_path`: modules importable from the
//! environment win, exactly like before the finder existed — hijacking them
//! would break native submodules resolved through their installed parent
//! (e.g. `yaml._yaml` under a bundled `yaml`). In isolated deployments — the
//! environments bundles actually target — nothing else provides the bundled
//! names and the finder serves them.
//!
//! Registrations store the init-function and namespace-variable NAMES and the
//! loader resolves them through `globals()` at import time, so the finder and
//! its registrations can be emitted before the definitions they refer to.

use ruff_python_ast::Stmt;

use super::CRIBO_SYS_ALIAS;

/// Python source of the finder infrastructure; parsed at generation time.
///
/// `{sys}` is replaced with the bundle's private `sys` alias.
const PRESERVED_FINDER_SOURCE: &str = r#"
class _CriboPreservedLoader:
    def __init__(self, entry):
        self._entry = entry

    def create_module(self, spec):
        return globals()[self._entry[1]]

    def exec_module(self, module):
        init = self._entry[0]
        if init is not None:
            globals()[init](module)


class _CriboPreservedFinder:
    def __init__(self):
        self._targets = {}

    def register(self, name, init, namespace, is_package):
        self._targets[name] = (init, namespace, is_package)

    def find_spec(self, name, path=None, target=None):
        entry = self._targets.get(name)
        if entry is None:
            return None
        from importlib.machinery import ModuleSpec
        return ModuleSpec(
            name,
            _CriboPreservedLoader(entry),
            is_package=entry[2],
        )


_cribo_finder = _CriboPreservedFinder()
{sys}.meta_path.append(_cribo_finder)
"#;

/// Generate the finder class, loader class, instance, and `sys.meta_path`
/// registration. Emitted once, only when preserved import targets exist.
pub(crate) fn generate_preserved_import_finder() -> Vec<Stmt> {
    use cow_utils::CowUtils;

    let source = PRESERVED_FINDER_SOURCE.cow_replace("{sys}", CRIBO_SYS_ALIAS);
    let parsed =
        ruff_python_parser::parse_module(&source).expect("preserved-finder source is valid Python");
    parsed.into_syntax().body.into_iter().collect()
}

/// Generate `_cribo_finder.register("name", "init_name", "namespace_name",
/// is_package)`. Names are strings resolved lazily through `globals()` by the
/// loader, so registrations may precede the definitions they refer to.
///
/// `init_function_name` is `None` for modules without an init function
/// (inlined ancestors of a preserved target): their code already ran at bundle
/// load, so the loader only needs to hand their namespace to the machinery.
pub(crate) fn generate_preserved_target_registration(
    module_name: &str,
    init_function_name: Option<&str>,
    namespace_variable: &str,
    is_package: bool,
) -> Stmt {
    use ruff_python_ast::ExprContext;

    use super::{expressions, statements};

    statements::expr(expressions::call(
        expressions::attribute(
            expressions::name("_cribo_finder", ExprContext::Load),
            "register",
            ExprContext::Load,
        ),
        vec![
            expressions::string_literal(module_name),
            init_function_name.map_or_else(expressions::none_literal, expressions::string_literal),
            expressions::string_literal(namespace_variable),
            expressions::bool_literal(is_package),
        ],
        vec![],
    ))
}
