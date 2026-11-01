//! The recipe that decides, from the dataset's own data, what the cluster path may draw and what
//! its mapping tables must say: the same code the converter's stage writes the mapping with and a
//! reader certifies it with. Nothing here depends on the converter, a renderer or the ESM
//! machinery; geometry, the stage and the publisher stay in the converter.
//!
//! * [`overrides`]: the minimum guards over the archived subrecords of a reference and its base.
//! * [`classify`]: the reference rows of the core database, the classifier's refusal rules, the
//!   snow source, the block of a reference.
//! * [`scope`]: the declared scope, as typed settings and as its canonical JSON.
//! * [`model_facts`]: what a converted GLB's JSON says (primitives, materials, textures).
//! * [`resolve`]: the model catalog over the published files and the per-reference snapshot.
//! * [`authority`]: [`authority::CoreAuthority`], the `AuthoritativeInputs` over all of it.

pub mod authority;
pub mod classify;
pub mod model_facts;
pub mod overrides;
pub mod resolve;
pub mod scope;
