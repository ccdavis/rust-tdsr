//! Speech synthesis system

pub mod synth;
pub mod buffer;
pub mod backends;

pub use synth::{Synth, SpeechCommand, create_synth};
pub use buffer::SpeechBuffer;
