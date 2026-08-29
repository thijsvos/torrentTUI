//! Rendering, split by screen region rather than by widget type. `layout` owns
//! the frame skeleton and the bars along its edges; `table` and `detail` fill
//! the middle depending on mode; `help` and `dialogs` are overlays drawn on top
//! of whatever is already there. `input`, `progress` and `util` are shared
//! pieces the others compose.
//!
//! Every renderer is a free function taking a `Frame` plus the state it needs —
//! there are no stateful widgets, so drawing order is fully determined by
//! `run_app`.

pub mod detail;
pub mod dialogs;
pub mod help;
pub mod input;
pub mod layout;
pub mod palette;
pub mod progress;
pub mod search;
pub mod table;
pub mod util;
