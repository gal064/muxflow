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
//! - A position in a set counts from zero, `.aria_position_in_set(index)`: the
//!   AT-SPI and Windows bridges add one. AccessKit reads a set's size only from
//!   the item's container, so a list or tab list sets `.aria_size_of_set(...)`.
//!   An Explorer tree item reports no size: its siblings share no container
//!   in a flat tree, and AccessKit ignores the item's own.
//! - Tab stops follow Tauri. Where Tauri roves (the tab strip, the Explorer
//!   tree, the panel tabs, the Settings sections) the set is one stop, on its
//!   selected item or cursor, and the arrows move within it. Where Tauri has a
//!   button per row (Git, agents, workspaces) each row is a stop, and a row
//!   taking the keyboard scrolls into view
//!   ([`crate::context_menu::reveal_when_focused`]). Up and Down move between
//!   agent and workspace rows ([`crate::workspaces::RowKeys`]), not Git rows,
//!   as in Tauri.
//! - A control whose right-click menu Tauri opens from the keyboard opens it
//!   on Shift+F10 or the Menu key ([`crate::context_menu::open`]). A menu the
//!   pointer opens has no item selected: Kit cannot select one unseen, as
//!   Tauri's focus without `:focus-visible` did.
//! - Gaps that are Kit's or AccessKit's, left open: a menu has no name; a
//!   separator and a destructive (red) item read as unnamed menu items; the
//!   Settings section list has no name (Kit's tab bar takes none); Kit's radio
//!   groups count positions from one, one too many; a dialog's outer frame has
//!   no name; Kit's slider takes no focus, so the sound volume is out of the
//!   keyboard's reach, and a screen reader's step moves it without the change
//!   event the setting is saved on; disabled controls read as enabled, and
//!   nothing reads as expanded, on AccessKit's Linux bridge.
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
