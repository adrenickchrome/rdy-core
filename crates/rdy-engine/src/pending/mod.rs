//! The "needs you" queue: the item shape and its card.
//!
//! One item is one thing that waits for a person. The item shape is the same on every surface
//! (D-JV6Z8): the queue lists items, and each item carries its card. This module holds the
//! shape ([`item`]) and the card that renders it ([`card`]). It is pure: it formats what the
//! queue computes and reads no store.
//!
//! The queue ([`queue`]) is built from four parts: batches and their exceptions (`queue`), open
//! questions (`questions`), tensions that need a person (`tensions`) and snoozes (`snooze`).
//!
//! The card layout follows cd-W4XVS (one item) and cd-2J1WR (a batch and its exceptions). The
//! module docs of [`card`] give the layout and the cut rule.

pub mod card;
pub mod item;
mod questions;
pub mod queue;
mod snooze;
mod tensions;

pub use card::{CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, card_hash, render, render_checked};
pub use item::{ExceptionKind, Fact, Item, ItemKind, ItemOption, RemovalCard, age};
pub use queue::{NotReady, Pending, PendingCounts, PendingQuery};
