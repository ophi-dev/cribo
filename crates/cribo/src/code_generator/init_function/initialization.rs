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
        // if getattr(self, "__initialized__", False):
        //     return self
        let check_initialized = ast_builder::statements::if_stmt(
            ast_builder::expressions::call(
                ast_builder::expressions::name("getattr", ExprContext::Load),
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
        // if getattr(self, "__initializing__", False):
        //     return self  # Return partial module in partially-initialized state
        let check_initializing = ast_builder::statements::if_stmt(
            ast_builder::expressions::call(
                ast_builder::expressions::name("getattr", ExprContext::Load),
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

        // Register the module in sys.modules before executing its body, exactly like
        // Python's import machinery, but ONLY for modules that inspect sys.modules
        // (self-references such as `sys.modules[__name__]`, membership checks):
        // registering every bundled module would shadow installed distributions that
        // native extensions re-import while the bundled copy is still initializing.
        // The import machinery reads `__spec__` unguarded on registered parents when
        // resolving real submodule imports, so it must exist (None is valid).
        // self.__spec__ = None
        // _sys.modules[self.__name__] = self
        if crate::visitors::utils::accesses_own_sys_modules_entry(&ast.body) {
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
