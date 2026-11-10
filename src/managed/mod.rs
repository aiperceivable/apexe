//! Managed execution: policy, planning, the session state machine and the
//! copy primitive.
//!
//! The v1 contract lives in `docs/managed-execution/`:
//!
//! * [`policy`] and [`effect`] implement the authorization model;
//! * [`form`] and [`plan`] implement the reviewed call shapes and the plan
//!   digest;
//! * [`session`] and [`protocol`] implement the call lifecycle and wire
//!   messages;
//! * [`backend`] reports what the selected isolation backend can enforce, and
//!   refuses a policy it cannot keep;
//! * [`copy`] is the `builtin.copy_create` primitive.
//!
//! The Linux namespace and Landlock setup is **not** in this module yet. The
//! isolation domain is the release gate that has to run in a Linux VM
//! (`docs/managed-execution/isolation-domain.md#release-boundary`); until it
//! exists, [`backend::probe`] reports the backend as unsupported on this host
//! and no managed session can start. That is deliberate: an implementation that
//! compiles a policy it cannot enforce would be the exact failure the
//! specification forbids.

pub mod backend;
pub mod copy;
pub mod effect;
pub mod error;
pub mod form;
pub mod json;
pub mod path;
pub mod plan;
pub mod policy;
pub mod protocol;
pub mod session;

pub use error::{ManagedError, ManagedErrorCode, Phase};
