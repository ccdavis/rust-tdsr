//! Speech synthesis system

pub mod backends;
pub mod buffer;
pub mod synth;

pub use buffer::SpeechBuffer;
pub use synth::{create_synth, SpeechCommand, Synth};
