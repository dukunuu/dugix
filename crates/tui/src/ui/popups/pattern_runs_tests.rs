//! The shared popup pattern, as Jobs, Processes, Sub-agents and Tool health follow it.
#![allow(unused_imports)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};
