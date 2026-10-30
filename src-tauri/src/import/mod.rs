//! Provider metadata imports, canonical persistence and conservative matching.
pub mod http;
pub mod jobs;
pub mod matching;
mod metadata_repair;
pub mod model;
pub mod playback;
pub mod providers;
mod public_apple;
pub(crate) mod public_http;
pub(crate) mod public_soundcloud;
pub(crate) mod public_spotify;
mod public_youtube;
pub mod urls;

pub use jobs::State;
