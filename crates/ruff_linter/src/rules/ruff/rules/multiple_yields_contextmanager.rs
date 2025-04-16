use ruff_diagnostics::{Diagnostic, Violation};
use ruff_macros::{derive_message_formats, ViolationMetadata};
use ruff_python_ast::helpers::map_callable;
use ruff_python_ast::identifier::Identifier;
use ruff_python_ast::AnyNodeRef;
use ruff_python_ast::{self as ast, visitor::source_order};

use crate::checkers::ast::Checker;
use crate::rules::ruff::rules::helpers::function_def_visit_preorder_except_body;

/// ## What it does
/// Checks that a function decorated with `contextlib.contextmanager` yields at most once.
///
/// ### Why is this bad?
/// A context manager must yield exactly once. Multiple yields cause a runtime error.
///
/// ## Example
/// ```python
/// @contextlib.contextmanager
/// def broken_context_manager():
///     print("Setting up")
///     yield "first value"  # This yield is expected
///     print("Cleanup")
///     yield "second value"  # This violates the protocol
/// ```
/// ## References
/// - [Python documentation: contextlib.contextmanager](https://docs.python.org/3/library/contextlib.html#contextlib.contextmanager)
/// - [Python documentation: contextlib.asynccontextmanager](https://docs.python.org/3/library/contextlib.html#contextlib.asynccontextmanager)
#[derive(ViolationMetadata)]
pub(crate) struct MultipleYieldsInContextManager;

impl Violation for MultipleYieldsInContextManager {
    const FIX_AVAILABILITY: ruff_diagnostics::FixAvailability =
        ruff_diagnostics::FixAvailability::None;

    #[derive_message_formats]
    fn message(&self) -> String {
        "Function decorated with `contextlib.contextmanager` may yield more than once".to_string()
    }
}

/// RUF060
pub(crate) fn multiple_yields_in_contextmanager(
    checker: &Checker,
    function_def: &ast::StmtFunctionDef,
) {
    if !is_contextmanager_decorated(function_def, checker) {
        return;
    }
    let mut path_tracker = YieldPathTracker::default();
    source_order::walk_body(&mut path_tracker, &function_def.body);

    if path_tracker.has_multiple_yields {
        checker.report_diagnostic(Diagnostic::new(
            MultipleYieldsInContextManager,
            function_def.identifier(),
        ));
    }
}

fn is_contextmanager_decorated(function_def: &ast::StmtFunctionDef, checker: &Checker) -> bool {
    function_def.decorator_list.iter().any(|decorator| {
        let callable = map_callable(&decorator.expression);
        checker
            .semantic()
            .resolve_qualified_name(callable)
            .is_some_and(|qualified| {
                matches!(
                    qualified.segments(),
                    ["contextlib", "contextmanager" | "asynccontextmanager"]
                )
            })
    })
}

struct YieldPathTracker {
    has_multiple_yields: bool,
    yield_stack: Vec<usize>,
    return_stack: Vec<bool>,
}

impl YieldPathTracker {
    fn would_increment_check_yield_stack_top(&mut self, by: usize) {
        match self.yield_stack.pop() {
            Some(root) => {
                if root + by > 1 {
                    self.has_multiple_yields = true;
                }
                self.yield_stack.push(root);
            }
            None => {
                debug_assert!(false, "Invalid yield stack size when traversing AST");
                self.yield_stack.push(0);
            }
        }
    }
    fn apply_increment_check_yield_stack_top(&mut self, by: usize) {
        match self.yield_stack.pop() {
            Some(root) => {
                let new_root = root + by;
                if new_root > 1 {
                    self.has_multiple_yields = true;
                }
                self.yield_stack.push(new_root);
            }
            None => {
                self.yield_stack.push(by);
                debug_assert!(false, "Invalid yield stack size when traversing AST");
            }
        }
    }

    fn check_then_null_yield_counts_top(&mut self) {
        match self.yield_stack.pop() {
            Some(root) => {
                if root > 1 {
                    self.has_multiple_yields = true;
                }
                self.yield_stack.push(0);
            }
            None => {
                self.yield_stack.push(0);
                debug_assert!(false, "Invalid yield stack size when traversing AST");
            }
        }
    }

    fn pop_yields(&mut self) -> usize {
        match self.yield_stack.pop() {
            Some(counts) => counts,
            None => {
                debug_assert!(false, "Invalid yield stack size when traversing AST");
                0
            }
        }
    }

    fn pop_returns(&mut self) -> bool {
        match self.return_stack.pop() {
            Some(returns) => returns,
            None => {
                debug_assert!(false, "Invalid return stack size when traversing AST");
                false
            }
        }
    }

    fn handle_exclusive_branches(&mut self, branch_count: usize) {
        let mut max_yields_return_branches = 0;
        let mut max_yields_no_return_branches = 0;
        for _ in 0..branch_count {
            let branch_yields = self.pop_yields();
            let branch_returns = self.pop_returns();
            if branch_returns {
                max_yields_return_branches = max_yields_return_branches.max(branch_yields);
            } else {
                max_yields_no_return_branches = max_yields_no_return_branches.max(branch_yields);
            }
        }
        self.would_increment_check_yield_stack_top(max_yields_return_branches);
        self.apply_increment_check_yield_stack_top(max_yields_no_return_branches);
    }

    fn push_tracking_frame(&mut self) {
        self.yield_stack.push(0);
        self.return_stack.push(false);
    }
}

impl Default for YieldPathTracker {
    fn default() -> Self {
        Self {
            has_multiple_yields: false,
            yield_stack: vec![0],
            return_stack: vec![false],
        }
    }
}

impl<'a> source_order::SourceOrderVisitor<'a> for YieldPathTracker {
    fn enter_node(&mut self, node: AnyNodeRef<'a>) -> source_order::TraversalSignal {
        if self.has_multiple_yields {
            return source_order::TraversalSignal::Skip;
        }
        match node {
            AnyNodeRef::StmtFor(_)
            | AnyNodeRef::StmtWhile(_)
            | AnyNodeRef::StmtTry(_)
            | AnyNodeRef::StmtIf(_)
            | AnyNodeRef::StmtMatch(_)
            | AnyNodeRef::MatchCase(_) => {
                // Track for primary control flow structures
                // Optional branches like else/finally clauses are handled in leave_node
                // Except is handled in leave node to maintain logical locality
                self.push_tracking_frame();
            }
            _ => {}
        }
        source_order::TraversalSignal::Traverse
    }

    fn leave_node(&mut self, node: AnyNodeRef<'a>) {
        match node {
            AnyNodeRef::StmtTry(try_stmt) => {
                // Finally is always executed, even if prior branches return
                // Other branches are skipped
                let finally_yields = self.pop_yields();
                let finally_returns = self.pop_returns();

                let else_yields = self.pop_yields();
                let else_returns = self.pop_returns();

                // We need to distinguish whether an except branch returns
                let mut max_except_yields_with_return = 0;
                let mut max_except_yields_no_return = 0;
                for _ in 0..try_stmt.handlers.len() {
                    let except_yields = self.pop_yields();
                    let except_returns = self.pop_returns();
                    if except_returns {
                        max_except_yields_with_return =
                            max_except_yields_with_return.max(except_yields);
                    } else {
                        max_except_yields_no_return =
                            max_except_yields_no_return.max(except_yields);
                    }
                }
                let max_except_yields =
                    max_except_yields_no_return.max(max_except_yields_with_return);

                let try_yields = self.pop_yields();
                let try_returns = self.pop_returns();

                if finally_returns {
                    // Finally always executes; can ignore earlier returns
                    let max_path_yields =
                        try_yields + max_except_yields.max(else_yields) + finally_yields;
                    self.would_increment_check_yield_stack_top(max_path_yields);
                    self.check_then_null_yield_counts_top();
                } else {
                    // If finally doesn't return handle different cases
                    // try else return finally
                    // try else finally
                    let max_path_yields_return_except =
                        try_yields + max_except_yields_with_return + finally_yields;
                    let max_path_yields_no_return_except =
                        try_yields + max_except_yields_no_return + finally_yields;

                    // Return in try
                    if try_returns {
                        // Check except cases and propagate non-returning
                        // except branch yield counts
                        self.would_increment_check_yield_stack_top(max_path_yields_return_except);
                        self.apply_increment_check_yield_stack_top(
                            max_path_yields_no_return_except,
                        );
                    } else if else_returns {
                        // No exception
                        self.would_increment_check_yield_stack_top(else_yields);

                        // With exceptions, else is not executed
                        // Check except branches and propagate
                        // non-returning
                        self.would_increment_check_yield_stack_top(max_path_yields_return_except);
                        self.apply_increment_check_yield_stack_top(
                            max_path_yields_no_return_except,
                        );
                    } else {
                        // No return in try-else-finally
                        // Check except branches and propagate
                        // non-returning
                        let max_path_yields_except_returns =
                            try_yields + max_except_yields_with_return + finally_yields;
                        self.would_increment_check_yield_stack_top(max_path_yields_except_returns);

                        let max_no_return_path_yields = try_yields
                            + max_path_yields_no_return_except.max(else_yields)
                            + finally_yields;
                        self.apply_increment_check_yield_stack_top(max_no_return_path_yields);
                    }
                }
            }
            AnyNodeRef::StmtIf(if_stmt) => {
                let branch_count = 1 + if_stmt.elif_else_clauses.len();
                self.handle_exclusive_branches(branch_count);
            }
            AnyNodeRef::StmtMatch(match_stmt) => {
                let branch_count = match_stmt.cases.len();
                self.handle_exclusive_branches(branch_count);
            }
            AnyNodeRef::StmtFor(_) | AnyNodeRef::StmtWhile(_) => {
                let else_yields = self.pop_yields();
                let else_returns = self.pop_returns();
                let body_yields = self.pop_yields();
                let _body_returns = self.pop_returns();

                // Yield in loop is likely to yield multiple times
                self.has_multiple_yields |= body_yields > 0;
                self.apply_increment_check_yield_stack_top(else_yields);
                if else_returns {
                    // If else returns, don't propagate yield count
                    self.check_then_null_yield_counts_top();
                }
            }
            _ => {}
        }
    }

    fn visit_expr(&mut self, expr: &'a ast::Expr) {
        match expr {
            ast::Expr::Yield(_) | ast::Expr::YieldFrom(_) => {
                if let Some(count) = self.yield_stack.last_mut() {
                    *count += 1;
                    if *count > 1 {
                        self.has_multiple_yields = true;
                    }
                }
            }
            _ => source_order::walk_expr(self, expr),
        }
    }

    fn visit_stmt(&mut self, stmt: &'a ast::Stmt) {
        match stmt {
            ast::Stmt::Return(_) => {
                if let Some(returns) = self.return_stack.last_mut() {
                    *returns = true;
                }
            }
            ast::Stmt::FunctionDef(nested) => {
                function_def_visit_preorder_except_body(nested, self);
            }
            ast::Stmt::While(loop_stmt @ ast::StmtWhile { body, orelse, .. }) => {
                let node = ruff_python_ast::AnyNodeRef::StmtWhile(loop_stmt);

                if self.enter_node(node).is_traverse() {
                    self.visit_body(body);
                    self.push_tracking_frame();
                    self.visit_body(orelse);
                    self.leave_node(node);
                }
            }
            ast::Stmt::For(loop_stmt @ ast::StmtFor { body, orelse, .. }) => {
                let node = ruff_python_ast::AnyNodeRef::StmtFor(loop_stmt);
                if self.enter_node(node).is_traverse() {
                    self.visit_body(body);
                    self.push_tracking_frame();
                    self.visit_body(orelse);
                    self.leave_node(node);
                }
            }
            ast::Stmt::If(
                if_stmt @ ast::StmtIf {
                    body,
                    elif_else_clauses,
                    ..
                },
            ) => {
                let node = ruff_python_ast::AnyNodeRef::StmtIf(if_stmt);
                if self.enter_node(node).is_traverse() {
                    self.visit_body(body);
                    for clause in elif_else_clauses {
                        self.push_tracking_frame();
                        self.visit_elif_else_clause(clause);
                    }
                    self.leave_node(node);
                }
            }
            ast::Stmt::Try(
                try_stmt @ ast::StmtTry {
                    body,
                    handlers,
                    orelse,
                    finalbody,
                    ..
                },
            ) => {
                let node = ruff_python_ast::AnyNodeRef::StmtTry(try_stmt);
                if self.enter_node(node).is_traverse() {
                    self.visit_body(body);
                    for handler in handlers {
                        self.push_tracking_frame();
                        self.visit_except_handler(handler);
                    }

                    self.push_tracking_frame();
                    self.visit_body(orelse);
                    self.push_tracking_frame();
                    self.visit_body(finalbody);
                    self.leave_node(node);
                }
            }
            _ => source_order::walk_stmt(self, stmt),
        }
    }
}
