//! Input handling and key bindings
//!
//! The input system uses a stack-based handler architecture where handlers
//! can be pushed/popped to create modal interfaces (config menu, copy mode, etc.)

pub mod handler;
pub mod keymap;
pub mod default_handler;
pub mod config_handler;
pub mod buffer_handler;
pub mod copy_handler;

pub use handler::{KeyHandler, HandlerAction, HandlerStack};
pub use keymap::{create_default_keymap, KeyAction};
pub use default_handler::DefaultKeyHandler;
