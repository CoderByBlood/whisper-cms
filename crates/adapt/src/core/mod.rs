pub mod content;
pub mod context;
pub mod error;
pub mod recommendation;

pub use content::ContentKind;
pub use context::{RequestContext, ResponseBodySpec, ResponseSpec};
pub use error::CoreError;
pub use recommendation::{
    BodyPatch, BodyPatchKind, DomOp, HeaderPatch, HeaderPatchKind, ModelPatch, Recommendations,
};
