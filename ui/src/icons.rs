//! Toolbar icons, embedded so the overlay ships as a single binary.
//!
//! GPUI renders an SVG as an alpha mask tinted with the element's text colour,
//! so the strokes only need to be opaque; their colour is ignored.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

pub const PEN: &str = "icons/pen.svg";
pub const ERASER: &str = "icons/eraser.svg";
pub const UNDO: &str = "icons/undo.svg";
pub const REDO: &str = "icons/redo.svg";

const OPEN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round">"#;

fn icon(paths: &str) -> Vec<u8> {
    format!("{OPEN}{paths}</svg>").into_bytes()
}

pub struct Icons;

impl AssetSource for Icons {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let paths = match path {
            PEN => {
                r#"<path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/><path d="m15 5 4 4"/>"#
            }
            ERASER => {
                r#"<path d="m7 21-4.3-4.3c-1-1-1-2.5 0-3.4l9.6-9.6c1-1 2.5-1 3.4 0l5.6 5.6c1 1 1 2.5 0 3.4L13 21"/><path d="M22 21H7"/><path d="m5 11 9 9"/>"#
            }
            UNDO => {
                r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5a5.5 5.5 0 0 1-5.5 5.5H11"/>"#
            }
            REDO => {
                r#"<path d="m15 14 5-5-5-5"/><path d="M20 9H9.5A5.5 5.5 0 0 0 4 14.5A5.5 5.5 0 0 0 9.5 20H13"/>"#
            }
            _ => return Ok(None),
        };
        Ok(Some(Cow::Owned(icon(paths))))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok([PEN, ERASER, UNDO, REDO].map(SharedString::from).to_vec())
    }
}
