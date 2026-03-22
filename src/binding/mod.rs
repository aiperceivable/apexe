pub mod binding_gen;
pub mod module_id;
pub mod schema_gen;
pub mod writer;

// Re-export key types for convenience
pub use binding_gen::{BindingGenerator, GeneratedBinding, GeneratedBindingFile};
pub use writer::BindingYAMLWriter;
