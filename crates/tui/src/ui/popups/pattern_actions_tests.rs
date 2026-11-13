//! The shared popup pattern, as Settings, Login, Local models, Copy, Connectors and Swarm follow it.
#![allow(unused_imports)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nexus_core::app::Popup;

use super::tests::{render_to_string, test_app};
