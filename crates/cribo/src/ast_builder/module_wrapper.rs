use ruff_python_ast::{Expr, ExprContext, Stmt};

use crate::{
    ast_builder::{expressions, statements},
    code_generator::module_registry::sanitize_module_name_for_identifier,
    python::constants::INIT_STEM,
};

/// The __init__ attribute name for module initialization
pub(crate) const MODULE_INIT_ATTR: &str = INIT_STEM;

/// Creates just the init function and __init__ assignment statements for a module
/// Returns a vector containing the init function definition and the __init__ assignment
pub(crate) fn create_init_function_statements(
    module_name: &str,
    init_func_name: &str,
    init_function_body: Stmt,
) -> Vec<Stmt> {
    let mut stmts = Vec::new();

    let module_var = sanitize_module_name_for_identifier(module_name);
    // Add init function
    stmts.push(init_function_body);

    // Attach the init function to the module's __init__ attribute
    let attach_init = statements::assign(
        vec![expressions::attribute(
            expressions::name(&module_var, ExprContext::Load),
            MODULE_INIT_ATTR,
            ExprContext::Store,
        )],
        expressions::name(init_func_name, ExprContext::Load),
    );
    stmts.push(attach_init);

    stmts
}

/// Creates a complete wrapper module with namespace, init function, and __init__ assignment
/// Returns a vector of statements that should be added to the bundle in order
/// If `init_function_body` is None, only creates the namespace without init function
pub(crate) fn create_wrapper_module(
    module_name: &str,
    init_func_name: &str,
    init_function_body: Option<Stmt>,
    is_package: bool,
) -> Vec<Stmt> {
    let mut stmts = Vec::new();

    let module_var = sanitize_module_name_for_identifier(module_name);

    // 1. Create namespace with __initializing__ and __initialized__ flags
    // module_var = types.SimpleNamespace(__name__='...', __initializing__=False,
    // __initialized__=False)
    let mut kwargs = vec![
        expressions::keyword(Some("__name__"), expressions::string_literal(module_name)),
        expressions::keyword(Some("__initializing__"), expressions::bool_literal(false)),
        expressions::keyword(Some("__initialized__"), expressions::bool_literal(false)),
    ];

    // Add __path__ for packages
    if is_package {
        kwargs.push(expressions::keyword(
            Some("__path__"),
            expressions::list(vec![], ExprContext::Load),
        ));
    }

    let namespace_stmt = statements::simple_assign(
        &module_var,
        expressions::call(expressions::simple_namespace_ctor(), vec![], kwargs),
    );
    stmts.push(namespace_stmt);

    // 1b. Capture the namespace OBJECT in the meta-path finder: runtime imports
    // must resolve it even when user code later rebinds the bundle-global name
    // (`globals()["helper"] = sentinel` before a preserved import)
    stmts.push(statements::expr(expressions::call(
        expressions::attribute(
            expressions::name("_cribo_finder", ExprContext::Load),
            "bind",
            ExprContext::Load,
        ),
        vec![
            expressions::string_literal(module_name),
            expressions::name(&module_var, ExprContext::Load),
        ],
        vec![],
    )));

    // 2. Add the init function definition and __init__ assignment if provided
    if let Some(init_body) = init_function_body {
        let init_stmts = create_init_function_statements(module_name, init_func_name, init_body);
        stmts.extend(init_stmts);
    }

    stmts
}

/// Creates a wrapper module initialization call statement.
///
/// This creates the standard pattern for initializing wrapper modules:
/// `module = module.__init__(module)`
///
/// # Arguments
/// * `module_name` - The sanitized module variable name
///
/// # Example
/// ```rust
/// // Creates: `compat = compat.__init__(compat)`
/// let stmt = create_wrapper_module_init_call("compat");
/// ```
pub(crate) fn create_wrapper_module_init_call(module_name: &str) -> Stmt {
    statements::assign(
        vec![expressions::name(module_name, ExprContext::Store)],
        expressions::call(
            expressions::attribute(
                expressions::name(module_name, ExprContext::Load),
                MODULE_INIT_ATTR,
                ExprContext::Load,
            ),
            vec![expressions::name(module_name, ExprContext::Load)],
            vec![],
        ),
    )
}

/// Wrap a bundled-module access expression with a `sys.modules` consult:
///
/// ```python
/// _cribo.importlib.import_module("pkg.sub") \
///     if "pkg" in _cribo.sys.modules or "pkg.sub" in _cribo.sys.modules \
///     else <access>
/// ```
///
/// CPython's `_find_and_load` returns an existing `sys.modules` entry before
/// invoking any finder or loader, and resolves DOTTED names through the
/// parent's `__path__` — so a preloaded replacement of the target OR of any
/// ancestor package must route through the real machinery rather than the
/// bundled access. When no component is preloaded, the direct bundled access
/// is used. The conditional (rather than `or`) also honors falsy replacement
/// objects.
pub(crate) fn sys_modules_consult_or(module_name: &str, access: Expr) -> Expr {
    use ruff_python_ast::BoolOp;

    let cribo_attribute = |attribute: &str| {
        expressions::attribute(
            expressions::name(super::CRIBO_PREFIX, ExprContext::Load),
            attribute,
            ExprContext::Load,
        )
    };
    let sys_modules =
        || expressions::attribute(cribo_attribute("sys"), "modules", ExprContext::Load);

    // "pkg" in _cribo.sys.modules or "pkg.mid" in ... or "pkg.mid.sub" in ...
    let mut component_tests = Vec::new();
    let mut boundary = 0_usize;
    loop {
        match module_name[boundary..].find('.') {
            Some(offset) => boundary += offset,
            None => boundary = module_name.len(),
        }
        component_tests.push(expressions::in_op(
            expressions::string_literal(&module_name[..boundary]),
            sys_modules(),
        ));
        if boundary == module_name.len() {
            break;
        }
        boundary += 1;
    }
    let any_component_preloaded = if component_tests.len() == 1 {
        component_tests
            .pop()
            .expect("one component test must exist")
    } else {
        expressions::bool_op(BoolOp::Or, component_tests)
    };

    expressions::if_exp(
        any_component_preloaded,
        expressions::call(
            expressions::attribute(
                cribo_attribute("importlib"),
                "import_module",
                ExprContext::Load,
            ),
            vec![expressions::string_literal(module_name)],
            vec![],
        ),
        access,
    )
}

/// Creates a wrapper module initialization call that honors preloaded
/// `sys.modules` entries, for modules whose entries consumer code observably
/// manipulates:
///
/// `module = <consult> else module.__init__(module)`
pub(crate) fn create_wrapper_module_init_call_honoring_sys_modules(
    module_var: &str,
    original_module_name: &str,
) -> Stmt {
    let init_call = expressions::call(
        expressions::attribute(
            expressions::name(module_var, ExprContext::Load),
            MODULE_INIT_ATTR,
            ExprContext::Load,
        ),
        vec![expressions::name(module_var, ExprContext::Load)],
        vec![],
    );
    statements::assign(
        vec![expressions::name(module_var, ExprContext::Store)],
        sys_modules_consult_or(original_module_name, init_call),
    )
}
