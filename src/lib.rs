//! scaffolder — a declarative project scaffolding CLI.
//!
//! Layered architecture: `cli → app/infra → domain`. The domain is pure (no dependencies) and
//! defines the port traits, infra implements the ports, app assembles the lifecycle from domain
//! ports only, and cli is the composition root.

pub mod app;
pub mod cli;
pub mod domain;
pub mod infra;
