//! Input handling and key bindings
//!
//! The input system uses a stack-based handler architecture where handlers
//! can be pushed/popped to create modal interfaces (config menu, copy mode, etc.)

pub mod buffer_handler;
pub mod config_handler;
pub mod copy_handler;
pub mod default_handler;
pub mod dispatch;
pub mod handler;
pub mod keymap;
pub mod tokenize;

pub use default_handler::DefaultKeyHandler;
pub use dispatch::dispatch_key;
pub use handler::{HandlerAction, HandlerStack, KeyHandler};
pub use keymap::{create_default_keymap, KeyAction};
pub use tokenize::{is_terminal_response, split_keys};
