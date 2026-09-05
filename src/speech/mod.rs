//! Speech synthesis system

pub mod backends;
pub mod buffer;
pub mod resample;
pub mod synth;
pub mod voices;

pub use buffer::SpeechBuffer;
pub use synth::{create_synth, SpeechCommand, Synth};
