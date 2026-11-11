//! Window chrome shared by every feature, and the accessibility baseline.
//!
//! # Accessibility baseline
//!
//! Packaged `cua-driver` journeys find and press controls through the
//! accessibility tree, so every control follows these rules. Screen-reader
//! access to terminal and Live Markdown text is an accepted exception.
//!
//! - Give a control a stable `.id(...)`, an AccessKit role and a name.
//!   Elements with no id or no role are left out of the tree.
//! - Kit controls set their role and read their name from `.label(...)`. An
//!   icon-only control needs `.accessibility_label(...)`.
//! - A plain `div` that acts as a control sets `.role(Role::Button)` and
//!   `.aria_label(...)`. Its `.on_click(...)` also answers a click through the
//!   tree. Any other action needs `.on_a11y_action(...)`.
//! - Reflect state with `.aria_selected(...)`, `.aria_expanded(...)` and
//!   `.aria_toggled(...)`.
//!
//! Static text needs none of this. A journey finds a control by its name: on
//! Linux the tree reports no role name and window-relative bounds, and
//! `cua-driver` refuses element tokens, so a journey clicks at the element's
//! window position, which reaches the element through the tree.
//!
//! Test with `gpui_kit::test::TestQueryExt` (`find_by_label`,
//! `find_all_by_role`), as `tests/shell.rs` does.

pub mod chrome;
pub mod error_summary;
pub mod icons;
pub mod shell;
pub mod surface_error;
pub mod title_bar;
pub mod toolbar;
