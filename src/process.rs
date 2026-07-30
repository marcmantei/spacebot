//! Process lifecycle concerns for the spacebot daemon.
//!
//! Spacebot spawns shell commands, coding agents, and build tools, and in a
//! container it is also PID 1. This module holds what that role requires —
//! today, reaping the orphans PID 1 inherits (see [`reaper`]).

pub mod reaper;
