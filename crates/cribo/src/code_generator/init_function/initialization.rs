//! Initialization phase for init function transformation
//!
//! This phase adds initialization guards and setup to the function body.

use log::debug;
use ruff_python_ast::{ExprContext, ModModule};

use super::state::InitFunctionState;
use crate::{
    ast_builder,
    code_generator::{
        bundler::Bundler,
        context::ModuleTransformContext,
        globals::GlobalsLifter,
        module_transformer::{SELF_PARAM, transform_ast_with_lifted_globals},
    },
};

/// Phase responsible for adding initialization guards and globals lifting
pub(crate) struct InitializationPhase;

impl InitializationPhase {
    /// Execute the initialization phase
    ///
    /// This phase adds:
    /// 1. __initialized__ check - return early if already initialized
    /// 2. __initializing__ check - return partial module if circular dependency
    /// 3. Set __initializing__ = True
    /// 4. Apply globals lifting if needed
    pub(crate) fn execute(
        bundler: &Bundler<'_>,
        ctx: &ModuleTransformContext<'_>,
        ast: &mut ModModule,
        state: &mut InitFunctionState,
    ) {
        // Add __initialized__ check
        // if _cribo_getattr(self, "__initialized__", False):
        //     return self
        // (the builtin is a definition-time captured keyword-only parameter)
        let check_initialized = ast_builder::statements::if_stmt(
            ast_builder::expressions::call(
                ast_builder::expressions::name(super::CAPTURED_GETATTR, ExprContext::Load),
                vec![
                    ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
                    ast_builder::expressions::string_literal("__initialized__"),
                    ast_builder::expressions::bool_literal(false),
                ],
                vec![],
            ),
            vec![ast_builder::statements::return_stmt(Some(
                ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
            ))],
            vec![],
        );
        state.body.push(check_initialized);

        // Add __initializing__ check (circular dependency guard)
        // if _cribo_getattr(self, "__initializing__", False):
        //     return self  # Return partial module in partially-initialized state
        let check_initializing = ast_builder::statements::if_stmt(
            ast_builder::expressions::call(
                ast_builder::expressions::name(super::CAPTURED_GETATTR, ExprContext::Load),
                vec![
                    ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
                    ast_builder::expressions::string_literal("__initializing__"),
                    ast_builder::expressions::bool_literal(false),
                ],
                vec![],
            ),
            vec![ast_builder::statements::return_stmt(Some(
                ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
            ))],
            vec![],
        );
        state.body.push(check_initializing);

        // Mark as initializing at the start of init to emulate Python's partial module semantics
        state.body.push(ast_builder::statements::assign_attribute(
            SELF_PARAM,
            "__initializing__",
            ast_builder::expressions::bool_literal(true),
        ));

        // Stamp __package__ with its real import-system value: a package is its own
        // package, a submodule belongs to its parent, a top-level module to "".
        // Body references to __package__ are rewritten to self.__package__, so
        // conditionals, registry keys, and logger names observe the original value
        // instead of the bundle entry's.
        let module_is_package = bundler.get_module_id(ctx.module_name).is_some_and(|id| {
            bundler.resolver.is_package_init(id) || bundler.resolver.is_namespace_package(id)
        });
        let package_value = if module_is_package {
            ctx.module_name.to_owned()
        } else {
            ctx.module_name
                .rsplit_once('.')
                .map(|(parent, _)| parent.to_owned())
                .unwrap_or_default()
        };
        state.body.push(ast_builder::statements::assign_attribute(
            SELF_PARAM,
            "__package__",
            ast_builder::expressions::string_literal(&package_value),
        ));

        // Stamp __doc__ with the module docstring (or None): the docstring
        // executes as an ordinary expression inside the init otherwise, and
        // SimpleNamespace would fall back to its type's documentation for
        // `provider.__doc__` reads.
        let docstring_value =
            crate::code_generator::docstring_extractor::extract_module_docstring(ast)
                .map_or_else(ast_builder::expressions::none_literal, |docstring| {
                    ast_builder::expressions::string_literal(&docstring)
                });
        state.body.push(ast_builder::statements::assign_attribute(
            SELF_PARAM,
            "__doc__",
            docstring_value,
        ));

        // Register the module in sys.modules before executing its body, exactly like
        // Python's import machinery, but ONLY for modules that inspect sys.modules
        // (self-references such as `sys.modules[__name__]`, membership checks) or
        // whose entry a CONSUMER observes (`sys.modules[dep.__name__]`, literal
        // keys): registering every bundled module would shadow installed
        // distributions that native extensions re-import while the bundled copy is
        // still initializing. Preserved import_module targets do NOT need this:
        // their calls run through the real import machinery (via the bundle's
        // meta-path finder), which manages sys.modules itself.
        // The import machinery reads `__spec__` unguarded on registered parents when
        // resolving real submodule imports, so it must exist (None is valid).
        // self.__spec__ = None
        // _sys.modules[self.__name__] = self
        if crate::visitors::utils::accesses_own_sys_modules_entry(&ast.body)
            || bundler
                .resolver
                .is_sys_modules_observed_target(ctx.module_name)
        {
            state.registers_in_sys_modules = true;
            state.body.push(ast_builder::statements::assign_attribute(
                SELF_PARAM,
                "__spec__",
                ast_builder::expressions::none_literal(),
            ));
            state.body.push(ast_builder::statements::assign(
                vec![ast_builder::expressions::subscript(
                    ast_builder::expressions::attribute(
                        ast_builder::expressions::name(
                            ast_builder::CRIBO_SYS_ALIAS,
                            ExprContext::Load,
                        ),
                        "modules",
                        ExprContext::Load,
                    ),
                    ast_builder::expressions::attribute(
                        ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
                        "__name__",
                        ExprContext::Load,
                    ),
                    ExprContext::Store,
                )],
                ast_builder::expressions::name(SELF_PARAM, ExprContext::Load),
            ));
        }

        // NOTE: We do NOT call parent init from child modules
        // In Python, the import machinery ensures parent is initialized before child,
        // but this happens OUTSIDE the child module's code.
        // Child modules don't explicitly call parent init - that would create
        // artificial circular dependencies.
        // The parent will be initialized by whoever imports the child module.

        // Apply globals lifting if needed
        state.lifted_names = ctx.global_info.as_ref().and_then(|global_info| {
            if global_info.global_declarations.is_empty() {
                None
            } else {
                let globals_lifter = GlobalsLifter::new(global_info);
                let lifted_names = globals_lifter.get_lifted_names().clone();

                // Transform the AST to use lifted globals
                transform_ast_with_lifted_globals(
                    bundler,
                    ast,
                    &lifted_names,
                    global_info,
                    Some(ctx.module_name),
                );

                debug!(
                    "Applied globals lifting for module '{}': {:?}",
                    ctx.module_name, lifted_names
                );

                Some(lifted_names)
            }
        });
    }
}
