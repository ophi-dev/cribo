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
//! Three registration forms exist:
//! - wrapper modules: init-function plus namespace-variable NAMES, resolved
//!   through `globals()` at import time (registrations may precede the
//!   definitions they refer to); the loader resets the wrapper state when the
//!   init fails so a retried import re-executes the body, like Python
//! - inlined modules with classes: an EXPORTS map from original attribute
//!   names to bundle-global binding names; the loader builds a namespace on
//!   demand exposing the very same objects (pickle identity)
//! - inlined ancestor packages: a namespace-variable name only; their code
//!   already ran at bundle load

use ruff_python_ast::Stmt;

use super::CRIBO_SYS_ALIAS;

/// Python source of the finder infrastructure; parsed at generation time.
///
/// `{sys}` is replaced with the bundle's private `sys` alias.
const PRESERVED_FINDER_SOURCE: &str = r#"
class _CriboPreservedLoader:
    # Pre-initialization namespace snapshots, keyed by namespace object id:
    # after sys.modules eviction, CPython executes a FRESH module, so the
    # re-import must not observe globals left over from the previous life
    _pristine = {}

    def __init__(self, entry, namespace=None):
        self._entry = entry
        self._namespace = namespace

    def create_module(self, spec):
        module = None
        if self._namespace is not None:
            module = self._namespace
        elif self._entry[1] is not None:
            module = globals()[self._entry[1]]
        if module is not None:
            if getattr(module, '_cribo_machinery_loaded', False):
                # Eviction re-import: CPython executes a FRESH module object,
                # and references to the EVICTED module keep observing its old
                # namespace; rebuild from the pre-initialization snapshot so
                # the two lives are distinct objects
                saved = type(self)._pristine.get(id(module))
                fresh = _cribo.types.SimpleNamespace()
                fresh.__dict__.update(
                    saved if saved is not None else {'__name__': spec.name}
                )
                return fresh
            return module
        module = _cribo.types.SimpleNamespace(__name__=spec.name)
        for export, binding in self._entry[3].items():
            setattr(module, export, globals()[binding])
        return module

    def exec_module(self, module):
        init = self._entry[0]
        if init is None:
            return
        # The class GLOBAL may be legally rebound or deleted by user code;
        # reach the shared snapshot store through the instance's own type
        pristine = type(self)._pristine
        key = id(module)
        if getattr(module, '_cribo_machinery_loaded', False):
            # Eviction re-import: restore the pre-initialization namespace so
            # the body re-executes against fresh state, like CPython's fresh
            # module object
            saved = pristine.get(key)
            if saved is not None:
                module.__dict__.clear()
                module.__dict__.update(saved)
            module.__initialized__ = False
            module.__initializing__ = False
        state = dict(module.__dict__)
        if key not in pristine:
            pristine[key] = dict(state)
        try:
            globals()[init](module)
            module._cribo_machinery_loaded = True
        except BaseException:
            # Python discards a failed module entirely; a retried import must
            # observe a FRESH namespace, not the partial mutations
            module.__dict__.clear()
            module.__dict__.update(state)
            module.__initializing__ = False
            raise


class _CriboPreservedFinder:
    def __init__(self):
        self._targets = {}
        self._namespaces = {}
        # Captured at definition time, in the bundle prelude: user code may
        # legally rebind or delete the module-level class name later
        self._loader = _CriboPreservedLoader

    def register(self, name, init, namespace, is_package, exports=None):
        self._targets[name] = (init, namespace, is_package, exports or {})

    def bind(self, name, namespace):
        # Namespace OBJECTS are captured where they are created, so later
        # rebinding of their bundle-global names cannot break imports
        self._namespaces[name] = namespace

    def find_spec(self, name, path=None, target=None):
        entry = self._targets.get(name)
        if entry is None:
            return None
        from importlib.machinery import ModuleSpec
        return ModuleSpec(
            name,
            self._loader(entry, self._namespaces.get(name)),
            is_package=entry[2],
        )


_cribo_finder = _CriboPreservedFinder()
{sys}.meta_path.append(_cribo_finder)
# First-party bundled TOP-LEVEL modules and packages keep their original
# precedence: the entry directory beat installed distributions before
# bundling, so their finder sits in FRONT of PathFinder (behind the
# builtin/frozen importers). Dotted submodules and third-party modules stay
# in the APPENDED finder, where installed distributions win (native
# submodules resolve through installed parents).
_cribo_finder_local = _CriboPreservedFinder()
_cribo_finder_local._namespaces = _cribo_finder._namespaces
for _cribo_index, _cribo_meta_finder in enumerate({sys}.meta_path):
    if getattr(_cribo_meta_finder, '__name__', '') == 'PathFinder':
        {sys}.meta_path.insert(_cribo_index, _cribo_finder_local)
        break
else:
    {sys}.meta_path.insert(0, _cribo_finder_local)
"#;

/// Generate the finder class, loader class, instance, and `sys.meta_path`
/// registration. Emitted once, only when bundled modules are registered.
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
/// (inlined ancestors of a registered module): their code already ran at
/// bundle load, so the loader only needs to hand their namespace to the
/// machinery.
///
/// `first_party` selects the finder: first-party TOP-LEVEL PLAIN MODULES
/// register with the finder in front of `PathFinder` (the entry directory
/// beat installed distributions before bundling, and such modules neither
/// have submodules nor resolve through an installed parent); packages, dotted
/// submodules, and third-party modules register with the appended one
/// (installed distributions win).
pub(crate) fn generate_preserved_target_registration(
    module_name: &str,
    init_function_name: Option<&str>,
    namespace_variable: &str,
    is_package: bool,
    first_party: bool,
) -> Stmt {
    use ruff_python_ast::ExprContext;

    use super::{expressions, statements};

    statements::expr(expressions::call(
        expressions::attribute(
            expressions::name(finder_variable(first_party), ExprContext::Load),
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

/// The bundle-global name of the finder serving a registration.
const fn finder_variable(first_party: bool) -> &'static str {
    if first_party {
        "_cribo_finder_local"
    } else {
        "_cribo_finder"
    }
}

/// Generate `_cribo_finder.register("name", None, None, is_package,
/// {"Export": "bundle_binding", ...})` for an INLINED module: the loader
/// builds a namespace on demand exposing the same top-level objects (class
/// identity for pickle and friends), since inlined code has no init function
/// or eager namespace object.
pub(crate) fn generate_inlined_module_registration(
    module_name: &str,
    exports: &[(String, String)],
    is_package: bool,
    first_party: bool,
) -> Stmt {
    use ruff_python_ast::{AtomicNodeIndex, DictItem, Expr, ExprContext, ExprDict};
    use ruff_text_size::TextRange;

    use super::{expressions, statements};

    let items = exports
        .iter()
        .map(|(export, binding)| DictItem {
            key: Some(expressions::string_literal(export)),
            value: expressions::string_literal(binding),
        })
        .collect();
    let exports_dict = Expr::Dict(ExprDict {
        node_index: AtomicNodeIndex::NONE,
        range: TextRange::default(),
        items,
    });

    statements::expr(expressions::call(
        expressions::attribute(
            expressions::name(finder_variable(first_party), ExprContext::Load),
            "register",
            ExprContext::Load,
        ),
        vec![
            expressions::string_literal(module_name),
            expressions::none_literal(),
            expressions::none_literal(),
            expressions::bool_literal(is_package),
            exports_dict,
        ],
        vec![],
    ))
}
