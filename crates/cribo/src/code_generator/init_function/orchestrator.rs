//! Orchestrator for coordinating init function transformation phases
//!
//! This module provides the `InitFunctionBuilder` which coordinates the execution
//! of all transformation phases to convert a Python module AST into an initialization
//! function.
//!
//! **STATUS**: ✅ Complete and Production Ready
//!
//! The orchestrator successfully coordinates all 12 phases to transform Python module ASTs
//! into initialization functions. All phases work together through explicit state transitions
//! via `InitFunctionState`, providing a clean, modular alternative to the previous monolithic
//! implementation.
//!
//! **Bug History**: During development, a bug was discovered where global variables showed
//! incorrect module names. This was caused by missing globals/locals transformation in the
//! Finalization phase. The bug was fixed by adding `transform_globals_in_stmt()` and
//! `transform_locals_in_stmt()` calls, and is now fully resolved (verified by all 148 tests).
//!
//! **Production Status**: The orchestrator is now the sole implementation used in production.
//! The original monolithic function has been deleted.

use ruff_python_ast::{ModModule, Stmt};

use super::{
    BodyPreparationPhase, CleanupPhase, FinalizationPhase, ImportAnalysisPhase,
    ImportTransformationPhase, InitFunctionState, InitializationPhase, StatementProcessingPhase,
    SubmoduleHandlingPhase, TransformError, WildcardImportPhase, WrapperGlobalsPhase,
    WrapperSymbolSetupPhase,
};
use crate::{
    code_generator::{bundler::Bundler, context::ModuleTransformContext},
    resolver::ModuleId,
    types::FxIndexMap,
};

/// Builder for coordinating the multi-phase transformation of a module AST
/// into an initialization function
pub(crate) struct InitFunctionBuilder<'a> {
    bundler: &'a Bundler<'a>,
    ctx: &'a ModuleTransformContext<'a>,
    symbol_renames: &'a FxIndexMap<ModuleId, FxIndexMap<String, String>>,
}

impl<'a> InitFunctionBuilder<'a> {
    /// Create a new builder with the required context
    pub(crate) const fn new(
        bundler: &'a Bundler<'a>,
        ctx: &'a ModuleTransformContext<'a>,
        symbol_renames: &'a FxIndexMap<ModuleId, FxIndexMap<String, String>>,
    ) -> Self {
        Self {
            bundler,
            ctx,
            symbol_renames,
        }
    }

    /// Build the initialization function by executing all transformation phases
    ///
    /// This method orchestrates the following phases in order:
    /// 1. Initialization - Add guards and handle globals lifting
    /// 2. Import Analysis - Analyze imports without modifying AST
    /// 3. Import Transformation - Transform imports in AST
    /// 4. Wrapper Symbol Setup - Create placeholder assignments
    /// 5. Wildcard Import Processing - Handle `from module import *`
    /// 6. Body Preparation - Analyze and process module body
    /// 7. Wrapper Globals Collection - Collect wrapper module globals
    /// 8. Statement Processing - Process each statement type with transformations
    /// 9. Submodule Handling - Set up submodule attributes
    /// 10. Final Cleanup - Add re-exports and explicit imports
    /// 11. Finalization - Create the function statement
    pub(crate) fn build(self, mut ast: ModModule) -> Result<Stmt, TransformError> {
        let mut state = InitFunctionState::new();

        // Phase 1: Initialization
        InitializationPhase::execute(self.bundler, self.ctx, &mut ast, &mut state);

        // Phase 2: Import Analysis
        ImportAnalysisPhase::execute(
            self.bundler,
            self.ctx,
            &ast,
            self.symbol_renames,
            &mut state,
        );

        // Phase 3: Import Transformation
        ImportTransformationPhase::execute(
            self.bundler,
            self.ctx,
            &mut ast,
            self.symbol_renames,
            &mut state,
        )?;

        // Phase 4: Wrapper Symbol Setup
        WrapperSymbolSetupPhase::execute(self.bundler, &mut state);

        // Phase 5: Wildcard Import Processing
        WildcardImportPhase::execute(self.bundler, self.ctx, &mut state);

        // Phase 6: Body Preparation
        // Clone lifted_names to avoid borrow conflict
        let lifted_names_for_prep = state.lifted_names.clone();
        let prep_context = BodyPreparationPhase::execute(
            self.bundler,
            self.ctx,
            &ast,
            &mut state,
            lifted_names_for_prep.as_ref(),
        );

        // Phase 7: Wrapper Globals Collection
        WrapperGlobalsPhase::execute(&prep_context.processed_body, &mut state);

        // Phase 8: Statement Processing
        StatementProcessingPhase::execute(prep_context, self.bundler, self.ctx, &mut state);

        // Phase 9: Submodule Handling
        SubmoduleHandlingPhase::execute(self.bundler, self.ctx, self.symbol_renames, &mut state);

        // Phase 10: Final Cleanup
        CleanupPhase::execute(self.bundler, self.ctx, &mut state);

        // Phase 11: Finalization
        let registers_in_sys_modules = state.registers_in_sys_modules;
        let function_stmt = FinalizationPhase::build_function_stmt(self.bundler, self.ctx, state)?;

        // For modules registered in sys.modules, mirror Python's failure semantics:
        // an exception during module execution unregisters the module and clears the
        // initializing guard so a later import retries instead of observing a stale,
        // partially initialized namespace
        Ok(Self::wrap_registered_init_with_failure_cleanup(
            function_stmt,
            registers_in_sys_modules,
        ))
    }

    /// Wrap a registered init function's body (everything after the registration
    /// prologue) in `try/except BaseException` that resets `__initializing__`,
    /// removes the `sys.modules` entry when it still points to `self`, and
    /// re-raises.
    fn wrap_registered_init_with_failure_cleanup(
        mut function_stmt: Stmt,
        registers_in_sys_modules: bool,
    ) -> Stmt {
        use ruff_python_ast::{CmpOp, ExceptHandler, ExceptHandlerExceptHandler, ExprContext};

        use crate::{
            ast_builder::{CRIBO_SYS_ALIAS, expressions, statements},
            code_generator::module_transformer::SELF_PARAM,
        };

        if !registers_in_sys_modules {
            return function_stmt;
        }
        let Stmt::FunctionDef(function_def) = &mut function_stmt else {
            return function_stmt;
        };
        // The registration prologue: __initialized__ guard, __initializing__ guard,
        // __initializing__ = True, __spec__ = None, sys.modules registration
        const PROLOGUE_STATEMENTS: usize = 5;
        if function_def.body.len() <= PROLOGUE_STATEMENTS {
            return function_stmt;
        }
        let mut guarded_body: Vec<Stmt> = function_def
            .body
            .split_off(PROLOGUE_STATEMENTS)
            .into_iter()
            .collect();

        let sys_modules = || {
            expressions::attribute(
                expressions::name(CRIBO_SYS_ALIAS, ExprContext::Load),
                "modules",
                ExprContext::Load,
            )
        };
        let self_name_attribute = || {
            expressions::attribute(
                expressions::name(SELF_PARAM, ExprContext::Load),
                "__name__",
                ExprContext::Load,
            )
        };
        // On success, honor the final sys.modules entry: a module may deliberately
        // replace itself (`sys.modules[__name__] = replacement`), and Python's import
        // machinery returns that replacement to importers.
        // return _sys.modules.get(self.__name__, self)
        if matches!(guarded_body.last(), Some(Stmt::Return(_))) {
            guarded_body.pop();
        }
        guarded_body.push(statements::return_stmt(Some(expressions::call(
            expressions::attribute(sys_modules(), "get", ExprContext::Load),
            vec![
                self_name_attribute(),
                expressions::name(SELF_PARAM, ExprContext::Load),
            ],
            vec![],
        ))));
        // self.__initializing__ = False
        // if _sys.modules.get(self.__name__) is self:
        //     del _sys.modules[self.__name__]
        // raise
        let registration_is_current =
            ruff_python_ast::Expr::Compare(ruff_python_ast::ExprCompare {
                node_index: ruff_python_ast::AtomicNodeIndex::NONE,
                left: Box::new(expressions::call(
                    expressions::attribute(sys_modules(), "get", ExprContext::Load),
                    vec![self_name_attribute()],
                    vec![],
                )),
                ops: Box::new([CmpOp::Is]),
                comparators: Box::new([expressions::name(SELF_PARAM, ExprContext::Load)]),
                range: ruff_text_size::TextRange::default(),
            });
        let unregister = Stmt::Delete(ruff_python_ast::StmtDelete {
            node_index: ruff_python_ast::AtomicNodeIndex::NONE,
            targets: vec![expressions::subscript(
                sys_modules(),
                self_name_attribute(),
                ExprContext::Del,
            )],
            range: ruff_text_size::TextRange::default(),
        });
        let cleanup = vec![
            statements::assign_attribute(
                SELF_PARAM,
                "__initializing__",
                expressions::bool_literal(false),
            ),
            statements::if_stmt(registration_is_current, vec![unregister], vec![]),
            statements::raise(None, None),
        ];
        let handler = ExceptHandler::ExceptHandler(ExceptHandlerExceptHandler {
            node_index: ruff_python_ast::AtomicNodeIndex::NONE,
            type_: Some(Box::new(expressions::name(
                super::CAPTURED_BASE_EXCEPTION,
                ExprContext::Load,
            ))),
            name: None,
            body: cleanup.into(),
            range: ruff_text_size::TextRange::default(),
        });
        function_def.body.push(statements::try_stmt(
            guarded_body,
            vec![handler],
            vec![],
            vec![],
        ));
        function_stmt
    }
}
