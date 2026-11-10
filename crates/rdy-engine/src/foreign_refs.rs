//! Foreign references (repo links, D-DAPR8, D-FK58A, D-8VJAZ): how a write cites another
//! project's delivered blocks, and the one check of held foreign pins.
//!
//! - An author writes `alias:id` or `alias:KEY`, in a `{{…}}` reference of a body or string
//!   field, or as a link target. The alias names a live `repo_link` block of the view. The id or
//!   key resolves in that link's current profile selection only: the newest version each live
//!   source of the repo delivered under the link's profile. Another profile's delivered blocks
//!   never widen it.
//! - The store keeps `{{<repo>:<id>}}` (a key typed bare keeps the `@key` mark) and a foreign
//!   link `{target: {repo, id}}`. A new citation pins the content of the selected current
//!   delivery. A citation the block already holds keeps its pin: repin, plan repin and reaffirm
//!   never move a foreign pin.
//! - Aliases are unique among live repo links, and a link never names this project's own repo.
//! - [`ReadState::check_held_pins`] is the one held-pin check: every foreign pin must name content
//!   a delivery before the state's watermark carried. A write applies it before it appends;
//!   replay, carry and adoption call it too.
use crate::error::*;
use crate::{Materialized, RdyError, ReadState, Scope};
use rdy_model::foreign::{ForeignId, ForeignPin};
use rdy_model::reference::{ForeignRef, foreign_ref, foreign_token};
use rdy_model::{Block, BlockId, RefMode, RepoId, key, ref_spans};
use rdy_schema::repo_link::{
    FIX as LINK_FIX, REPO_LINK, REPO_LINK_PREFIX, is_valid_alias, is_valid_repo_id,
    message as link_message,
};
use std::cell::{OnceCell, RefCell};
use std::collections::BTreeMap;

/// The repo link reason of an alias another live link holds.
pub(crate) const REASON_ALIAS_IN_USE: &str = "alias is already in use";
/// The repo link reason of a link to this project's own repo.
pub(crate) const REASON_OWN_REPO: &str = "repo is this project";

/// The hex digits a repo with no live alias shows as.
const SHORT_HEX: usize = 12;

/// One live repo link: its block, the repo it cites and the profile it reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LinkRow {
    pub(crate) link: BlockId,
    pub(crate) repo: RepoId,
    pub(crate) profile: String,
}

/// The live repo links of a state, by alias. A link whose alias or repo the schema refuses is
/// left out; of two links with one alias (a state no write makes), the lower id counts.
#[derive(Clone, Debug, Default)]
pub(crate) struct RepoLinks {
    by_alias: BTreeMap<String, LinkRow>,
}

/// The alias and repo of `b` when it is a repo link with a valid alias and repo.
fn link_of(b: &Block) -> Option<(&str, RepoId)> {
    if b.block_type != REPO_LINK {
        return None;
    }
    let alias = b.field_str("alias").filter(|a| is_valid_alias(a))?;
    let repo = b
        .field_str("repo")
        .filter(|r| is_valid_repo_id(r))?
        .parse()
        .ok()?;
    Some((alias, repo))
}

impl RepoLinks {
    /// The repo links among `blocks`.
    pub(crate) fn of<'a>(blocks: impl IntoIterator<Item = &'a Block>) -> RepoLinks {
        let mut by_alias: BTreeMap<String, LinkRow> = BTreeMap::new();
        for b in blocks {
            let Some((alias, repo)) = link_of(b) else {
                continue;
            };
            let row = LinkRow {
                link: b.id.clone(),
                repo,
                profile: b.field_str("profile").unwrap_or_default().to_string(),
            };
            match by_alias.get(alias) {
                Some(have) if have.link <= row.link => {}
                _ => {
                    by_alias.insert(alias.to_string(), row);
                }
            }
        }
        RepoLinks { by_alias }
    }

    /// The repo links of `m`, read by the `rl` id prefix.
    pub(crate) fn of_view(m: &Materialized) -> RepoLinks {
        RepoLinks::of(
            m.ids()
                .filter(|id| id.prefix() == REPO_LINK_PREFIX)
                .filter_map(|id| m.get(id)),
        )
    }

    pub(crate) fn get(&self, alias: &str) -> Option<&LinkRow> {
        self.by_alias.get(alias)
    }

    /// Every live link with its alias, in alias order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (&str, &LinkRow)> {
        self.by_alias.iter().map(|(a, r)| (a.as_str(), r))
    }

    /// The live aliases of `repo`, in sorted order.
    fn aliases_of<'a>(&'a self, repo: &'a RepoId) -> impl Iterator<Item = (&'a str, &'a LinkRow)> {
        self.by_alias
            .iter()
            .filter(move |(_, r)| r.repo == *repo)
            .map(|(a, r)| (a.as_str(), r))
    }

    /// How `repo` shows: its first live alias in sorted order, else its first 12 hex digits.
    pub(crate) fn shown(&self, repo: &RepoId) -> String {
        match self.aliases_of(repo).next() {
            Some((a, _)) => a.to_string(),
            None => repo.to_string()[..SHORT_HEX].to_string(),
        }
    }
}

/// `E_UNKNOWN_REF`: no live repo link has `alias`.
pub(crate) fn unknown_alias(alias: &str) -> RdyError {
    RdyError::new(
        E_UNKNOWN_REF,
        format!("no repo link has the alias {alias}"),
        "use the alias of a linked project: rdy_links / rdy links",
    )
}

/// `E_UNKNOWN_REF`: `target` (an id or key as written) is not in what `alias` delivers.
pub(crate) fn not_delivered(alias: &str, target: &str) -> RdyError {
    RdyError::new(
        E_UNKNOWN_REF,
        format!("{alias}:{target} is not in the content delivered from {alias}"),
        format!(
            "cite a block that {alias} delivers: rdy_links {{op: \"show\", alias}} / rdy links show {alias}"
        ),
    )
}

/// `E_FOREIGN_VERSION`: no delivery holds the content `content` of `shown:id`.
pub(crate) fn missing_version(shown: &str, id: &BlockId, content: &rdy_model::Hash) -> RdyError {
    let short = &content.to_string()[..SHORT_HEX];
    RdyError::new(
        E_FOREIGN_VERSION,
        format!("{shown}:{id} has no delivered version {short}"),
        format!(
            "wait for the delivery that holds it, or pin a delivered version: rdy_links {{op: \"show\", alias}} / rdy links show {shown}"
        ),
    )
}

/// `E_VALIDATION`: repo link `alias` fails with `reason`.
fn bad_link(alias: &str, reason: &str) -> RdyError {
    RdyError::new(E_VALIDATION, link_message(alias, reason), LINK_FIX)
}

/// The refusal of a written repo link: its alias is held by another live link of the state the
/// write leaves (`links`, every live repo link block), or its repo is this project's (`own`).
pub(crate) fn check_link<'a>(
    b: &Block,
    own: Option<RepoId>,
    links: impl IntoIterator<Item = &'a Block>,
) -> Result<(), RdyError> {
    let Some((alias, repo)) = link_of(b) else {
        return Ok(());
    };
    if own == Some(repo) {
        return Err(bad_link(alias, REASON_OWN_REPO));
    }
    let taken = links
        .into_iter()
        .filter(|o| o.id != b.id)
        .any(|o| link_of(o).is_some_and(|(a, _)| a == alias));
    if taken {
        return Err(bad_link(alias, REASON_ALIAS_IN_USE));
    }
    Ok(())
}

/// The foreign side of one write: the view's live links, the state's foreign cut, and the pins
/// of the citations this write resolved from an alias.
pub(crate) struct ForeignCite<'a> {
    st: &'a ReadState,
    base: &'a Materialized,
    links: OnceCell<RepoLinks>,
    /// The pin of each target an alias resolved, from the alias's current selection.
    fresh: RefCell<BTreeMap<ForeignId, ForeignPin>>,
}

impl<'a> ForeignCite<'a> {
    pub(crate) fn new(st: &'a ReadState, base: &'a Materialized) -> ForeignCite<'a> {
        ForeignCite {
            st,
            base,
            links: OnceCell::new(),
            fresh: RefCell::new(BTreeMap::new()),
        }
    }

    /// The live repo links of the view before the write.
    pub(crate) fn links(&self) -> &RepoLinks {
        self.links.get_or_init(|| RepoLinks::of_view(self.base))
    }

    /// The read state the write starts from.
    pub(crate) fn state(&self) -> &'a ReadState {
        self.st
    }

    /// This project's repo id.
    pub(crate) fn own_repo(&self) -> Result<RepoId, RdyError> {
        self.st.repo_id()
    }

    /// The target `alias:target` names in the alias's current profile selection, and whether
    /// `target` was a key. Records the pin a new citation of it takes.
    fn resolve(&self, alias: &str, target: &str) -> Result<(ForeignId, bool), RdyError> {
        let row = self
            .links()
            .get(alias)
            .ok_or_else(|| unknown_alias(alias))?;
        let foreign = self.st.foreign()?;
        let sel = foreign.current_for_profile(&row.repo, &row.profile);
        let (id, by_key) = if BlockId::is_valid(target) {
            let id = BlockId::parse(target).expect("a valid id parses");
            (id, false)
        } else {
            let folded = key::fold(target);
            let mut found: Vec<BlockId> = vec![];
            for (id, held) in &sel {
                let b = self.st.reader().get_block(&held.version.version)?;
                if b.field_str("key").is_some_and(|k| key::fold(k) == folded) {
                    found.push((*id).clone());
                }
            }
            match crate::keys::sole_holder(target, found.iter())? {
                Some(id) => (id, true),
                None => return Err(not_delivered(alias, target)),
            }
        };
        let held = sel.get(&id).ok_or_else(|| not_delivered(alias, target))?;
        let fid = ForeignId::new(row.repo, id);
        self.fresh
            .borrow_mut()
            .entry(fid.clone())
            .or_insert(ForeignPin {
                content: held.version.content,
                encoding: held.version.encoding,
            });
        Ok((fid, by_key))
    }

    /// The pin a new citation of `target` takes: the one its alias resolved in this write, else
    /// (a citation written in the stored form) the current delivery of the first live alias of
    /// its repo, in sorted order, whose profile selection holds it, else, for an external
    /// decision, the current delivery of its ADR sources.
    pub(crate) fn new_pin(&self, target: &ForeignId) -> Result<ForeignPin, RdyError> {
        if let Some(pin) = self.fresh.borrow().get(target) {
            return Ok(pin.clone());
        }
        let foreign = self.st.foreign()?;
        let links = self.links();
        let mut first = None;
        for (alias, row) in links.aliases_of(&target.repo) {
            first.get_or_insert(alias);
            let sel = foreign.current_for_profile(&row.repo, &row.profile);
            if let Some(held) = sel.get(&target.id) {
                return Ok(ForeignPin {
                    content: held.version.content,
                    encoding: held.version.encoding,
                });
            }
        }
        if let Some(alias) = first {
            return Err(not_delivered(alias, target.id.as_str()));
        }
        // rpl13: an external decision is pinned to what its ADR sources deliver now
        if let Some((name, _)) = crate::foreign_guard::adr_source(foreign, &target.repo) {
            return match foreign.current_version(target) {
                Some(held) => Ok(ForeignPin {
                    content: held.version.content,
                    encoding: held.version.encoding,
                }),
                None => Err(not_delivered(name, target.id.as_str())),
            };
        }
        Err(unknown_alias(&links.shown(&target.repo)))
    }

    /// `text` with every `{{alias:id}}` and `{{alias:KEY}}` reference (as `mode` reads them)
    /// stored as `{{<repo>:<id>}}`. A key typed bare keeps the key-typed mark, and display text
    /// is kept. Stored-form and local references are copied unchanged.
    pub(crate) fn rewrite(&self, text: &str, mode: RefMode) -> Result<String, RdyError> {
        if mode == RefMode::Off || !text.contains(':') || !text.contains("{{") {
            return Ok(text.to_string());
        }
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for span in ref_spans(text, mode) {
            let Some(ForeignRef::Alias { alias, target }) = foreign_ref(&span.token) else {
                continue;
            };
            let (fid, by_key) = self.resolve(alias, target)?;
            let mut stored = span.clone();
            stored.key_typed = span.key_typed || (by_key && span.display.is_none());
            out.push_str(&text[last..span.range.start]);
            out.push_str(&stored.encode(&foreign_token(&fid.repo, &fid.id)));
            last = span.range.end;
        }
        out.push_str(&text[last..]);
        Ok(out)
    }

    /// The foreign target a link target or a reference token names: `None` for a local one.
    pub(crate) fn link_target(&self, text: &str) -> Option<Result<ForeignId, RdyError>> {
        Some(match foreign_ref(text.trim())? {
            ForeignRef::Alias { alias, target } => self.resolve(alias, target).map(|(f, _)| f),
            ForeignRef::Stored { repo, id } => Ok(ForeignId::new(repo, id)),
        })
    }

    /// The held-pin check of [`ReadState::check_held_pins`], with repos shown as the view
    /// before the write shows them.
    pub(crate) fn check_held<'b>(
        &self,
        blocks: impl IntoIterator<Item = &'b Block>,
    ) -> Result<(), RdyError> {
        check_held(self.st, blocks, &|r| self.links().shown(r))
    }
}

/// The first foreign pin among `blocks` whose content no delivery before `st`'s watermark held
/// is `E_FOREIGN_VERSION`, its repo shown by `shown`.
fn check_held<'b>(
    st: &ReadState,
    blocks: impl IntoIterator<Item = &'b Block>,
    shown: &dyn Fn(&RepoId) -> String,
) -> Result<(), RdyError> {
    for b in blocks {
        for (target, pin) in &b.foreign.pins {
            if st.foreign_held_pin(target, pin)?.is_none() {
                return Err(missing_version(
                    &shown(&target.repo),
                    &target.id,
                    &pin.content,
                ));
            }
        }
    }
    Ok(())
}

impl ReadState {
    /// The one held-pin check (repo links, D-FK58A): every foreign pin of `blocks` names content
    /// that a delivery before this state's watermark carried for its block, whatever source
    /// carried it and whether that source is live, revoked or no longer selects it. The first
    /// pin that fails is `E_FOREIGN_VERSION`, its repo shown by its first live alias in `scope`.
    /// Writes apply it before they append; replay, carry and adoption call it before any
    /// mutation.
    pub fn check_held_pins<'b>(
        &self,
        scope: &Scope,
        blocks: impl IntoIterator<Item = &'b Block>,
    ) -> Result<(), RdyError> {
        let links: OnceCell<Result<RepoLinks, RdyError>> = OnceCell::new();
        let shown = |r: &RepoId| -> String {
            match links.get_or_init(|| self.materialized(scope).map(|m| RepoLinks::of_view(&m))) {
                Ok(l) => l.shown(r),
                Err(_) => RepoLinks::default().shown(r),
            }
        };
        check_held(self, blocks, &shown)
    }
}
