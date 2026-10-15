//! The "needs you" queue: the item shape and its card.
//!
//! One item is one thing that waits for a person. The item shape is the same on every surface
//! (D-JV6Z8): the queue lists items, and each item carries its card. This module holds the
//! shape ([`item`]) and the card that renders it ([`card`]). It is pure: it formats what the
//! queue computes and reads no store.
//!
//! The card layout follows cd-W4XVS (one item) and cd-2J1WR (a batch and its exceptions). The
//! module docs of [`card`] give the layout and the cut rule.

pub mod card;
pub mod item;

pub use card::{CARD_DEFAULT_TOKENS, CARD_MAX_TOKENS, card_hash, render};
pub use item::{Fact, Item, ItemKind, ItemOption, age};
