//! Rule Engine Module
//!
//! Executes user-defined rules against network traffic flows.
//!
//! - `model`: Core data structures (Rule, Action, Filter) - defined in parent module.
//! - `compiler`: Compiles high-level Rules into optimized `CompiledRule` structures.
//! - `matcher`: Matches `CompiledFilter` against `Flow` data.
//! - `executor`: Main execution loop, iterates rules, checks stages, and triggers actions.
//! - `actions`: Implementation of specific actions (e.g., Modify Header, Drop, Mock).
//! - `validator`: Validates if filters/actions are appropriate for the current execution stage.

pub mod compiled;
pub mod compiler;
pub mod matcher;
pub mod validator;
pub mod executor;
pub mod actions;
pub mod state;

pub use executor::{RuleEngine, ExecutionContext};
