//! The shared popup pattern, as Help, Context, Usage, Research, Image viewer, API key, Tool approval and the interactive panel follow it.
#![allow(unused_imports)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};
