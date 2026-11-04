//! The bounded M3 scenario driver (G3). `script::m3_scenarios` schedules each forced route; the
//! writer it names runs it here, on its own bound connection, after the act it drew at the
//! scheduled step (`agent::spawn_paced`). Every act that answers, lands, checks, writes outputs
//! or compacts is a real tool exchange of that agent, or of an eliciting client the agent opens
//! for the person; the simulated person only chooses an option on the card it was shown and
//! gives its own words. Engine calls seed nothing here.
//!
//! Journal conventions:
//! - Each route attempt is one entry of its act (`Entry::from_route`): the observed ref and
//!   version, the card hash shown and the option chosen, the counts, and the reply the daemon
//!   gave (an acknowledgement and its record ids, a refusal code, or an interruption). The
//!   entry's `reply` is the tool's whole reply.
//! - A setup call (a write, a propose, a removal request, a log read, a compact snapshot) is an
//!   `M3_FIXTURE` entry naming its view; no refusal is allowed for it.
//! - Every read of the person's queue that shows a card before an answer is a `LAND_CARD` entry
//!   naming the version, whose `reply` holds `{via, shown, not_ready, past_cap}`: the card's ref, hash, options
//!   and recommended option as the queue gave them. A new card is read and shown before every
//!   answer, the second answer of a moved card too.
//! - A card past the queue's cap (`PENDING_ITEMS_MAX`, canon: an answer binds only an item
//!   inside it) is not skipped: the person first works down the oldest items of the queue
//!   (blocking tensions dismissed, batches approved) through the usual person routes, on the
//!   person's relay session and in the landing turn,
//!   bounded, each one an `m3_work_down` entry (`people::work_down`, deep-drain ruling finding
//!   2); a card that still does not fit fails the run by the route's act.
//! - A landing an answer made is found and audited as every other is (`people::found_in`): a
//!   `land_found` entry names it with the view and base read before it (`run::before_land`).
//!   A call the kill cuts is journaled `Lost` (`Reply::Interrupted`); the link reconnects
//!   through the normal proxy and session, the driver reads whether the version landed, and it
//!   never sends the answer again.
//!
//! The routes:
//! - `RelayedAnswer`: the agent relays the person's words with `rdy_answer` on the card shown.
//! - `WrongThenRightRole`: not run. The swarm has one person in one daemon, so no second person
//!   can hold the card's policy role. The authority area proves the route
//!   (`mg5_roles_route_and_enforce_the_same_person`); a `Skipped` entry of each act says so
//!   (`COVERED_BY_AREA`) and is never counted as a witness.
//! - `ElicitationRevisions`: one version answered through an eliciting client of each revision
//!   (2025-11-25 in-call, 2026-07-28 inline); the accepted answer lands as `local`.
//! - `DeclineThenRelay`: the person declines the elicited card, which stays open; a separate
//!   scripted answer of the person is then relayed. A decline authorizes nothing.
//! - `MovedCard`: the card of the first version is shown, a second version moves it, the answer
//!   on the old hash is refused (`E_PROPOSAL_MOVED`), and the new card is shown and answered.
//! - `PublicationCheckOutput`: the check of canon while the agents work, then the check and
//!   the outputs of canon's newest landing (a named, held scope), kept for the oracle.
//! - `RealCompact`: the person's scripted removal of the subject's claim, approved on its card,
//!   then `rdy_compact {dry_run: false}`, which must count a removed payload. The removal window
//!   holds the landing and plan turns (known issue TASK-1233: any write while a removal request
//!   is open makes every queue read fail), and the snapshots before and after the real compact
//!   are taken in the landing turn; the real compact runs while the agents work.
use super::agent::{AgentReport, Link, Res, answer_plan, answer_plan_held, scratch_editorial};
use super::journal::{Entry, Outcome, RouteAttempt, RouteCounts, Via};
use super::oracle::copy_tree;
use super::people::{self, accept_deferrals, landing_fields};
use super::project::{SeedBlocks, run_parent};
use super::run::{self, Before, Watch};
use super::script::{Route, Scenario};
use crate::mcp::{DEADLINE, McpClient, PersonAct, Reply, Revision};
use crate::publication_scenario::{scan_local_links, tree};
use rdy_engine::pending::CARD_MAX_TOKENS;
use rdy_store::layout::STORE_DIR;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

/// The act of a read of the person's queue that shows a card before an answer.
pub const LAND_CARD: &str = "land_card";
/// The act of a scenario's setup call.
pub const M3_FIXTURE: &str = "m3_fixture";
/// Why `WrongThenRightRole` is not run in the swarm, and where its route is proven (ruling
/// task-8, finding 1). A `Skipped` entry is never a witness.
pub const COVERED_BY_AREA: &str =
    "wrong-role route: one swarm person; proven by mg5_roles_route_and_enforce_the_same_person";
/// The chat session the person speaks in.
const SESSION_REF: &str = "chat";
/// The publication profile the publication route checks: every doc, audience org.
pub const PROFILE: &str = "implicit:org";

/// Called with the route and the act just before each route attempt's call: a test cuts the
/// daemon there.
pub type CallHook = Arc<dyn Fn(Route, &'static str) + Send + Sync>;

/// What a run gives the agents for the M3 routes: the scheduled scenarios and a test's hook.
#[derive(Clone, Default)]
pub struct M3Hooks {
    pub scenarios: Vec<Scenario>,
    pub before_call: Option<CallHook>,
}

impl M3Hooks {
    /// The scenarios agent `i` runs after its act of `step`.
    pub fn at(&self, i: usize, step: usize) -> Vec<Scenario> {
        self.scenarios
            .iter()
            .filter(|s| s.agent == i && s.step == step)
            .cloned()
            .collect()
    }
}

/// The word an approval's label begins with, as the card shows it: the person looks for it.
pub const APPROVE: &str = "Approve";
/// The first line of each removal request's section on a card.
pub use rdy_engine::pending::card::REMOVAL_HEADING as REMOVAL_SECTION;
/// What a cut card says where it left lines out.
const CUT: &str = " cut; rdy_pending with a larger max_tokens shows them)";

/// A card as the person's queue showed it: the whole item, and what the person reads of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub r#ref: String,
    pub card_hash: String,
    pub title: String,
    /// The card's text at the largest budget: what the person reads.
    pub text: String,
    /// The option ids, in the card's order.
    pub options: Vec<String>,
    /// The option labels, in the card's order.
    pub labels: Vec<String>,
    /// The id of the option the card recommends, if one.
    pub recommended: Option<String>,
    /// Whether the same read listed the version's deferral card (`defer:<P-x@n>`), which the
    /// person accepts before the approval (D-AAQ9Z).
    pub deferral: bool,
    /// The queue item whole, as `rdy_pending` gave it.
    pub item: Value,
}

/// `text` with every run of white space as one space, as the card renders a value.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Card {
    /// Whether the text the person reads is the whole card of the item (fix round 1, finding
    /// 4): its first line names the ref and title, nothing is cut, the summary, every option's
    /// numbered label and each of its consequences stand in it, and a removal card shows each
    /// request's section with its warnings. What is missing, if anything.
    pub fn complete(&self) -> Result<(), String> {
        let t = &self.text;
        if t.trim().is_empty() {
            return Err(format!("{}: the card has no text", self.r#ref));
        }
        let head = format!("{}  {}", self.r#ref, one_line(&self.title));
        if !t.lines().next().is_some_and(|l| l.starts_with(&head)) {
            return Err(format!("the first line is not {head:?}: {t}"));
        }
        if t.contains(CUT) {
            return Err(format!("the card is cut: {t}"));
        }
        let flat = one_line(t);
        if let Some(summary) = self.item["summary"].as_str().map(one_line)
            && !summary.is_empty()
            && !flat.contains(&summary)
        {
            return Err(format!("the summary {summary:?} is not shown: {t}"));
        }
        if self.options.is_empty() {
            return Err(format!("the card offers no option: {t}"));
        }
        let opts = self.item["options"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        for (n, o) in opts.iter().enumerate() {
            let label = one_line(o["label"].as_str().unwrap_or_default());
            let line = format!("  {}. {label}", n + 1);
            if label.is_empty() || !t.lines().any(|l| l.starts_with(&line)) {
                return Err(format!(
                    "the option {label:?} is not numbered {}: {t}",
                    n + 1
                ));
            }
            for c in o["consequences"].as_array().into_iter().flatten() {
                let c = one_line(c.as_str().unwrap_or_default());
                if !flat.contains(&c) {
                    return Err(format!("the consequence {c:?} is not shown: {t}"));
                }
            }
        }
        let requests = self.item["removal"].as_array().map_or(0, Vec::len);
        if requests > 0 {
            let sections = t.lines().filter(|l| *l == REMOVAL_SECTION).count();
            if sections < requests {
                return Err(format!(
                    "{requests} removal requests, {sections} sections shown: {t}"
                ));
            }
            for w in [
                rdy_engine::W_REDACTION_COPIES_MESSAGE,
                rdy_engine::W_REDACTION_QUOTES_MESSAGE,
                rdy_engine::W_REDACTION_REUSE_MESSAGE,
            ] {
                if !t.contains(w) {
                    return Err(format!("the warning {w:?} is not shown: {t}"));
                }
            }
        }
        Ok(())
    }

    /// The card as the `land_card` entry journals it: everything the person was shown.
    fn shown(&self) -> Value {
        let options: Vec<Value> = self
            .options
            .iter()
            .zip(&self.labels)
            .map(|(id, label)| json!({"id": id, "label": label}))
            .collect();
        json!({"ref": self.r#ref, "card_hash": self.card_hash, "title": self.title,
            "card": self.text, "options": options, "recommended": self.recommended,
            "deferral": self.deferral, "item": self.item})
    }
}

/// What a read of the person's queue showed of one version.
pub(crate) enum Shown {
    Card(Box<Card>),
    /// No card of the version: `not_ready` when the queue lists it as not ready, `past_cap` when
    /// the queue leaves items out past its cap.
    Absent {
        not_ready: bool,
        past_cap: bool,
    },
    /// The daemon died under the read; the link has reconnected.
    Lost,
}

/// The card of `pref` in a queue reply `q`.
fn card_in(q: &Value, pref: &str) -> Option<Card> {
    let i = q["items"].as_array()?.iter().find(|i| i["ref"] == pref)?;
    let options = i["options"].as_array()?;
    let field = |o: &Value, k: &str| o[k].as_str().unwrap_or_default().to_string();
    Some(Card {
        r#ref: i["ref"].as_str()?.to_string(),
        card_hash: i["card_hash"].as_str()?.to_string(),
        title: field(i, "title"),
        text: field(i, "card"),
        options: options.iter().map(|o| field(o, "id")).collect(),
        labels: options.iter().map(|o| field(o, "label")).collect(),
        recommended: options
            .iter()
            .find(|o| o["recommended"] == true)
            .and_then(|o| o["id"].as_str().map(str::to_string)),
        deferral: q["items"]
            .as_array()?
            .iter()
            .any(|i| i["ref"] == format!("defer:{pref}")),
        item: i.clone(),
    })
}

/// The person's queue read whole on `link` (`rdy_pending`, every card at its largest budget), as
/// a `LAND_CARD` entry naming `pref`: what it shows of `pref`.
pub(crate) fn show_card(link: &mut Link, pref: &str) -> Shown {
    let mut e = link.entry(LAND_CARD);
    e.block = Some(pref.to_string());
    let mut shown = None;
    let args = json!({"session_ref": SESSION_REF, "max_tokens": CARD_MAX_TOKENS});
    let res = link.exec(e, LAND_CARD, "rdy_pending", args, |en, v| {
        let card = card_in(v, pref);
        let not_ready = v["not_ready"]
            .as_array()
            .is_some_and(|n| n.iter().any(|x| x["ref"] == pref));
        let past_cap = v["omitted"].as_u64().unwrap_or(0) > 0;
        en.reply = Some(json!({
            "via": "rdy_pending",
            "shown": card.as_ref().map(Card::shown),
            "warnings": v["warnings"],
            "not_ready": not_ready,
            "past_cap": past_cap,
        }));
        shown = Some(match card {
            Some(c) => Shown::Card(Box::new(c)),
            None => Shown::Absent {
                not_ready,
                past_cap,
            },
        });
    });
    match res {
        Res::Ok(_) => {
            let shown = shown.expect("an Ok read fills it");
            // the person answers only a card shown whole (fix round 1, finding 4)
            if let Shown::Card(c) = &shown
                && let Err(why) = c.complete()
            {
                link.fail(
                    LAND_CARD,
                    &format!("the card of {pref} is not whole: {why}"),
                );
            }
            shown
        }
        Res::Lost => Shown::Lost,
        // the table allows no refusal of the read: `finish` failed the run
        Res::Refused(..) => unreachable!("a refused queue read fails the run"),
    }
}

/// The simulated person's choice on `card`: the option whose label, numbered in the text the
/// person read, begins with `word`. Its id is the id of the option of that number, and only when
/// that option's label is the one the text shows: the choice follows the card as presented,
/// never an id the person was not shown.
pub(crate) fn choose(card: &Card, word: &str) -> Option<String> {
    let mut lines = card.text.lines().skip_while(|l| *l != "Options").skip(1);
    let (n, shown) = lines.find_map(|l| {
        let (n, rest) = l.strip_prefix("  ")?.split_once(". ")?;
        let n: usize = n.parse().ok()?;
        rest.starts_with(word).then(|| (n, rest.to_string()))
    })?;
    let label = one_line(card.labels.get(n.checked_sub(1)?)?);
    (label.starts_with(word) && shown.starts_with(&label)).then(|| card.options[n - 1].clone())
}

/// The version number of a ref `P-x@n`.
fn version_of(pref: &str) -> &str {
    pref.rsplit_once('@').map_or("", |(_, n)| n)
}

/// The landing an approval's reply names, as an acknowledgement.
fn landing_ack(v: &Value) -> (String, Vec<String>) {
    let l = landing_fields(v)["landing"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let records = if l.is_empty() {
        vec![]
    } else {
        vec![l.clone()]
    };
    (l, records)
}

/// `rdy mcp --project <dir>` as the person's environment: a bounded session's proxy to the
/// project's daemon.
fn proxy(dir: &Path, env: &[(String, String)]) -> Command {
    let mut cmd = crate::fixtures::rdy_command();
    cmd.args(["mcp", "--project"])
        .arg(dir)
        .env_remove("RDY_ACTOR")
        .env_remove("RDY_RUN")
        .env_remove("RDY_NO_DAEMON")
        .envs(env.iter().map(|(k, v)| (k, v)));
    cmd
}

/// The 2025-11-25 in-call flow with the form capability.
pub fn in_call() -> Revision {
    Revision::Initialize {
        version: "2025-11-25".into(),
        capabilities: json!({"elicitation": {"form": {}}}),
    }
}

/// The 2026-07-28 inline flow (no `initialize`) with the form capability.
pub fn inline() -> Revision {
    Revision::Inline {
        version: "2026-07-28".into(),
        capabilities: json!({"elicitation": {"form": {}}}),
    }
}

/// Runs scenario `sc` on agent `link`'s connection (see the module docs).
pub(crate) fn run(
    link: &mut Link,
    seed: &SeedBlocks,
    sc: &Scenario,
    watch: Option<&Watch>,
    hooks: &M3Hooks,
    report: &mut AgentReport,
) {
    let start = link.calls;
    let mut c = Ctx {
        link,
        seed,
        sc,
        watch,
        hooks,
        report,
        start,
        elicited: 0,
        relay: None,
    };
    match sc.route {
        Route::RelayedAnswer => c.relayed(),
        Route::WrongThenRightRole => c.covered_by_area(),
        Route::ElicitationRevisions => c.revisions(),
        Route::DeclineThenRelay => c.decline_then_relay(),
        Route::MovedCard => c.moved(),
        Route::PublicationCheckOutput => c.publication(),
        Route::RealCompact => c.compact(),
    }
}

struct Ctx<'a> {
    link: &'a mut Link,
    seed: &'a SeedBlocks,
    sc: &'a Scenario,
    watch: Option<&'a Watch>,
    hooks: &'a M3Hooks,
    report: &'a mut AgentReport,
    /// The link's call count when the scenario began.
    start: usize,
    /// The calls the scenario's eliciting clients made.
    elicited: usize,
    /// The person's relay session, opened when a card is past the queue's cap.
    relay: Option<Link>,
}

impl Ctx<'_> {
    /// Fails the run by the scenario's name once it has made more calls than its bound.
    fn bound(&self) {
        let made = self.link.calls - self.start + self.elicited;
        if made > self.sc.max_calls {
            self.link.fail(
                self.sc.route.name(),
                &format!(
                    "scenario {} made {made} calls, past its bound of {}",
                    self.sc.route.name(),
                    self.sc.max_calls
                ),
            );
        }
    }

    /// The agent's name, `a<i>`.
    fn me(&self) -> String {
        self.link.actor.trim_start_matches("agent:").to_string()
    }

    /// The scenario's own view: `a<i>-m3<tag>`, so the drain finds its owner.
    fn view(&mut self, tag: &str) -> String {
        let v = format!("{}-m3{tag}", self.me());
        self.report.views.insert(v.clone());
        v
    }

    /// The claim the route owns, or the subject's claim `n`.
    fn claim(&self, n: usize) -> String {
        let (subject, _) = self.sc.route.subject();
        self.seed.subject(subject).claims[n].clone()
    }

    /// A setup call, journaled as `M3_FIXTURE` naming `view`; no refusal is allowed.
    fn fixture(
        &mut self,
        view: Option<&str>,
        tool: &str,
        args: Value,
        fill: impl FnOnce(&mut Entry, &Value),
    ) -> Res {
        let mut e = self.link.entry(M3_FIXTURE);
        e.view = view.map(str::to_string);
        let res = self.link.exec(e, M3_FIXTURE, tool, args, fill);
        self.bound();
        res
    }

    /// One new version of `view`: claim `claim` gets `text` (an editorial write), the plan is
    /// answered, and the view is proposed with a summary. `held`: the caller holds the landing
    /// and plan turns. `None` when the kill cut a call.
    fn version(&mut self, view: &str, claim: &str, text: &str, held: bool) -> Option<String> {
        let gate = self.link.gate.clone();
        {
            let _act = (!held).then(|| gate.act_turn());
            let ops = json!([{"op": "update", "id": claim, "body": text}]);
            let args = json!({"ops": ops, "editorial": scratch_editorial(), "view": view});
            let res = self.fixture(Some(view), "rdy_write", args, |en, v| {
                en.change = v["change"].as_str().map(str::to_string);
                en.block = Some(claim.to_string());
            });
            if !matches!(res, Res::Ok(_)) {
                return None;
            }
        }
        self.propose(view, held)
    }

    /// Answers `view`'s plan and proposes it, as an agent's propose does: the read turn, then
    /// the plan turn for the answers. The version's ref.
    fn propose(&mut self, view: &str, held: bool) -> Option<String> {
        let gate = self.link.gate.clone();
        let _read = (!held).then(|| gate.read_turn());
        if held {
            answer_plan_held(self.link, view);
        } else {
            answer_plan(self.link, view);
        }
        let mut made = None;
        let args = json!({"view": view, "title": view,
            "summary": format!("The M3 scenario {view}.")});
        let res = self.fixture(Some(view), "rdy_propose", args, |en, v| {
            en.block = v["ref"].as_str().map(str::to_string);
            made = en.block.clone();
        });
        matches!(res, Res::Ok(_)).then_some(made).flatten()
    }

    /// The attempt of `act` on `pref`, through `via`, showing `card` with `option`.
    fn attempt(
        &self,
        act: &'static str,
        via: Via,
        pref: &str,
        shown: Option<(&Card, &str)>,
    ) -> RouteAttempt {
        let (run, _) = self.link.session();
        let a = RouteAttempt::begin(self.sc.route, act, &self.link.actor, run, via)
            .unwrap_or_else(|| self.link.fail(act, "is not an act of its route"))
            .observed(pref, version_of(pref));
        match shown {
            Some((card, option)) => a.shown(&card.card_hash, option),
            None => a,
        }
    }

    /// The test's hook, just before the call of `act`.
    fn before(&self, act: &'static str) {
        if let Some(h) = &self.hooks.before_call {
            h(self.sc.route, act);
        }
    }

    /// Journals attempt `a` with the reply `res` (`ack` reads an acknowledgement from an `Ok`
    /// reply) and checks the refusal against the act's closed set; after a cut call the link
    /// reconnects.
    fn journal(
        &mut self,
        a: RouteAttempt,
        res: Res,
        ack: impl FnOnce(&Value) -> (String, Vec<String>),
    ) -> Res {
        let act = a.act;
        let a = match &res {
            Res::Ok(v) => {
                let (id, records) = ack(v);
                a.acknowledged(&id, records)
                    .unwrap_or_else(|| self.link.fail(act, &format!("acknowledged nothing: {v}")))
            }
            Res::Refused(code, _) => a.refused(code),
            Res::Lost => a.interrupted(),
        };
        let e = Entry::from_route(self.link.step, a).expect("an attempt with a reply");
        let res = self
            .link
            .finish(e, act, res, |en, v| en.reply = Some(v.clone()));
        self.bound();
        res
    }

    /// After an answer on `pref`: an acknowledged approval is found landed and audited; a cut
    /// answer is read on the restarted daemon and never sent again. Whether it landed.
    fn settle(&mut self, act: &'static str, pref: &str, before: Before, res: &Res) -> bool {
        let landed = match res {
            Res::Ok(_) => {
                if !people::found_in(self.link, self.watch, pref, before, false) {
                    self.link
                        .fail(act, &format!("acknowledged {pref}, which did not land"));
                }
                true
            }
            Res::Lost => people::found_in(self.link, self.watch, pref, before, false),
            Res::Refused(..) => false,
        };
        self.bound();
        landed
    }

    /// A `Skipped` entry of `act`: the route could not take its next step, for `why`.
    fn gap(&mut self, act: &'static str, pref: Option<&str>, why: &str) {
        let mut e = self.link.entry(act);
        e.outcome = Outcome::Skipped;
        e.block = pref.map(str::to_string);
        e.message = Some(why.to_string());
        self.link.journal.record(e);
    }

    /// The card of `pref` as the queue shows it (`show_card`). When it is past the queue's cap,
    /// the person first works down the oldest items through the usual person routes
    /// (`people::work_down`, deep-drain ruling finding 2) on its own relay session, in the
    /// landing turn (`held`: the caller holds it), and the card is read again.
    fn show(&mut self, act: &str, pref: &str, held: bool) -> Shown {
        match show_card(self.link, pref) {
            Shown::Absent { past_cap: true, .. } => {
                let gate = self.link.gate.clone();
                let _turn = (!held).then(|| gate.landing_turn());
                let step = self.link.step;
                let link = &*self.link;
                let relay = self.relay.get_or_insert_with(|| {
                    Link::connect(
                        link.at.clone(),
                        "agent:relay",
                        "chat",
                        "reviewer",
                        None,
                        link.journal.clone(),
                        link.gate.clone(),
                    )
                });
                relay.step = step;
                people::work_down(relay, self.watch, pref, act);
                show_card(self.link, pref)
            }
            shown => shown,
        }
    }

    /// The reads every approval makes first (`run::before_land`).
    fn before_answer(&mut self, pref: &str) -> Option<Before> {
        let before = run::before_land(self.link, pref);
        self.bound();
        before
    }

    /// The card of `pref` read and shown before an answer of `act`. When the same read lists the
    /// version's deferral card, the person accepts it first (`accept_deferrals`, D-AAQ9Z) and the
    /// card is read and shown again. `None` (a `Skipped` entry of `act` when nothing is shown)
    /// when there is no card to answer or the kill cut a read.
    fn card(&mut self, act: &'static str, pref: &str) -> Option<Card> {
        for accepted in [false, true] {
            match self.show(act, pref, true) {
                Shown::Card(c) if c.deferral && !accepted => {
                    let actor = self.link.actor.clone();
                    if !accept_deferrals(self.link, &actor, pref, true) {
                        return None;
                    }
                    self.bound();
                }
                Shown::Card(c) => {
                    self.bound();
                    return Some(*c);
                }
                Shown::Absent { not_ready, .. } => {
                    let why = format!("no card of {pref} (not ready: {not_ready})");
                    self.gap(act, Some(pref), &why);
                    return None;
                }
                Shown::Lost => return None,
            }
        }
        None
    }

    /// The card of `pref` is read and shown, the person chooses `approve` and gives `words`,
    /// and the agent relays them with `rdy_answer` as attempt `act`. Whether it landed. The
    /// caller holds the landing turn.
    fn relay(&mut self, act: &'static str, pref: &str, before: Before, words: &str) -> bool {
        let Some(card) = self.card(act, pref) else {
            return false;
        };
        let Some(option) = choose(&card, APPROVE) else {
            self.gap(act, Some(pref), "the card offers no approve");
            return false;
        };
        let a = self.attempt(act, Via::Tool("rdy_answer"), pref, Some((&card, &option)));
        let args = json!({"ref": card.r#ref, "option": option, "card_hash": card.card_hash,
            "evidence": words, "question": format!("Shall I land {pref}?"),
            "session_ref": SESSION_REF});
        self.before(act);
        let res = self.link.raw(act, "rdy_answer", args);
        let res = self.journal(a, res, landing_ack);
        self.settle(act, pref, before, &res)
    }

    /// The person's answer `answer` to the card of `pref` through an eliciting client of
    /// `revision`, the agent's own actor and run on a connection of its own, as attempt `act`.
    /// The daemon's reply; a client that broke is a cut call.
    fn elicit(
        &mut self,
        act: &'static str,
        pref: &str,
        card: &Card,
        revision: Revision,
        answer: Value,
        choice: &str,
    ) -> Res {
        let mut a = self.attempt(act, Via::Tool("rdy_answer"), pref, Some((card, choice)));
        let mut client = match McpClient::try_launch_eliciting(
            proxy(&self.link.at.dir, &self.link.at.env),
            revision,
        ) {
            Ok(c) => c,
            Err(f) => {
                let res = self.link.lost(act, &format!("the eliciting client: {f:?}"));
                return self.journal(a, res, |_| (String::new(), vec![]));
            }
        };
        let (run, role) = self.link.session();
        let session = json!({"actor": self.link.actor, "run": run, "role": role});
        match client.try_call("rdy_session", session) {
            Ok(Reply::Ok(_)) => {}
            Ok(other) => self
                .link
                .fail(act, &format!("the eliciting session: {other:?}")),
            Err(f) => {
                let res = self
                    .link
                    .lost(act, &format!("the eliciting session: {f:?}"));
                return self.journal(a, res, |_| (String::new(), vec![]));
            }
        }
        let mut forms = 0;
        let mut person = |_: &Value| {
            forms += 1;
            PersonAct::Answer(answer.clone())
        };
        let args = json!({"ref": card.r#ref, "card_hash": card.card_hash, "elicit": true});
        self.before(act);
        let got = client.try_call_elicited("rdy_answer", args, &mut person);
        self.elicited += client.calls();
        a = a.counted(RouteCounts {
            pages: 1,
            requests: forms,
            answers: forms,
            ..RouteCounts::default()
        });
        let res = match got {
            Ok(Reply::Ok(v)) => Res::Ok(v),
            Ok(Reply::ToolError(v)) => Res::Refused(
                v["code"].as_str().unwrap_or("?").to_string(),
                v["message"].as_str().unwrap_or_default().to_string(),
            ),
            Ok(other) => self.link.fail(act, &format!("got {other:?}")),
            Err(f) => self.link.lost(act, &format!("the elicited call: {f:?}")),
        };
        if matches!(res, Res::Ok(_) | Res::Refused(..)) {
            // a clean end of the session; its failure after a full reply changes nothing
            let _ = client.try_finish(DEADLINE);
        }
        if let Res::Ok(v) = &res
            && forms == 0
        {
            self.link.fail(
                act,
                &format!("the elicited answer asked the person nothing: {v}"),
            );
        }
        self.journal(a, res, |v| {
            if v["state"].is_string() {
                (v["state"].as_str().unwrap_or_default().to_string(), vec![])
            } else {
                landing_ack(v)
            }
        })
    }

    // ---- the routes ----

    fn relayed(&mut self) {
        let view = self.view("r");
        let claim = self.claim(self.sc.slot);
        let text = format!("The authority note of {view} holds as relayed.");
        let Some(pref) = self.version(&view, &claim, &text, false) else {
            return;
        };
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        let Some(before) = self.before_answer(&pref) else {
            return;
        };
        self.relay(
            "m3_relay_answer",
            &pref,
            before,
            &format!("Yes, land {pref}."),
        );
    }

    fn covered_by_area(&mut self) {
        for act in self.sc.route.acts() {
            self.gap(act, None, COVERED_BY_AREA);
        }
    }

    fn revisions(&mut self) {
        let claim = self.claim(self.sc.slot);
        let revisions = [
            ("e1", "m3_elicit_first_rev", in_call()),
            ("e2", "m3_elicit_second_rev", inline()),
        ];
        for (tag, act, revision) in revisions {
            let view = self.view(tag);
            let text = format!("The prompt note of {view} holds as elicited.");
            let Some(pref) = self.version(&view, &claim, &text, false) else {
                return;
            };
            let gate = self.link.gate.clone();
            let _turn = gate.landing_turn();
            let Some(before) = self.before_answer(&pref) else {
                return;
            };
            let Some(card) = self.card(act, &pref) else {
                return;
            };
            if choose(&card, APPROVE).is_none() {
                self.gap(act, Some(&pref), "the card offers no approve");
                return;
            }
            let accept = json!({"action": "accept", "content": {"choice": "approve"}});
            let res = self.elicit(act, &pref, &card, revision, accept, "approve");
            // an accepted continuation records local elicited evidence (D-6M0AF)
            if let Res::Ok(v) = &res
                && landing_fields(v)["assurance"] != "local"
            {
                self.link
                    .fail(act, &format!("an elicited approval that is not local: {v}"));
            }
            if !self.settle(act, &pref, before, &res) || matches!(res, Res::Lost) {
                return;
            }
        }
    }

    fn decline_then_relay(&mut self) {
        let view = self.view("d");
        let claim = self.claim(self.sc.slot);
        let text = format!("The prompt note of {view} holds after a decline.");
        let Some(pref) = self.version(&view, &claim, &text, false) else {
            return;
        };
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        let Some(before) = self.before_answer(&pref) else {
            return;
        };
        let Some(card) = self.card("m3_elicit_decline", &pref) else {
            return;
        };
        let decline = json!({"action": "decline"});
        let res = self.elicit(
            "m3_elicit_decline",
            &pref,
            &card,
            in_call(),
            decline,
            "decline",
        );
        match &res {
            Res::Ok(v) if v["state"] == "declined" => {}
            Res::Ok(v) => self
                .link
                .fail("m3_elicit_decline", &format!("a decline recorded {v}")),
            Res::Lost => {
                // whether a cut decline wrote anything is read; it is never sent again
                people::found_in(self.link, self.watch, &pref, before, false);
                return;
            }
            Res::Refused(..) => return,
        }
        // the decline authorizes nothing: the person answers again in words of its own, and the
        // agent relays that separate answer on the card it shows again
        self.relay(
            "m3_relay_after_decline",
            &pref,
            before,
            &format!("I have read {pref} now. Land it."),
        );
    }

    fn moved(&mut self) {
        let view = self.view("m");
        let claim = self.claim(self.sc.slot);
        let first = format!("The authority note of {view} holds, first.");
        let Some(old) = self.version(&view, &claim, &first, false) else {
            return;
        };
        let card = match self.show("m3_moved_card", &old, false) {
            Shown::Card(c) => *c,
            Shown::Absent { not_ready, .. } => {
                self.gap(
                    "m3_moved_card",
                    Some(&old),
                    &format!("no card (not ready: {not_ready})"),
                );
                return;
            }
            Shown::Lost => return,
        };
        let second = format!("The authority note of {view} holds, second.");
        let Some(newer) = self.version(&view, &claim, &second, false) else {
            return;
        };
        if newer == old {
            self.link
                .fail("m3_moved_card", &format!("{view}: {old} again"));
        }
        // the answer on the card the person saw, which the newer version moved
        let act = "m3_moved_card";
        let Some(option) = choose(&card, APPROVE) else {
            self.gap(act, Some(&old), "the card offers no approve");
            return;
        };
        let a = self.attempt(act, Via::Tool("rdy_answer"), &old, Some((&card, &option)));
        let args = json!({"ref": card.r#ref, "option": option, "card_hash": card.card_hash,
            "evidence": format!("Yes, land {old}."), "question": format!("Shall I land {old}?"),
            "session_ref": SESSION_REF});
        self.before(act);
        let res = self.link.raw(act, "rdy_answer", args);
        match self.journal(a, res, landing_ack) {
            Res::Refused(code, _) if code == "E_PROPOSAL_MOVED" => {}
            Res::Ok(v) => self.link.fail(act, &format!("the moved card landed: {v}")),
            // cut: nothing is sent again; the view drains
            Res::Lost => return,
            Res::Refused(code, m) => self.link.fail(act, &format!("got {code}: {m}")),
        }
        // a new card is shown before another answer, and the person chooses again
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        let Some(before) = self.before_answer(&newer) else {
            return;
        };
        self.relay(
            act,
            &newer,
            before,
            &format!("Yes, land {newer} as it reads now."),
        );
    }

    /// One publication call as attempt `act`, through `tool`; `out` is the output it writes, if
    /// any, whose local links are then checked.
    fn publish(&mut self, act: &'static str, tool: &'static str, args: Value, out: Option<&Path>) {
        let scope = args["scope"].as_str().unwrap_or("canon").to_string();
        let mut a = self.attempt(act, Via::Tool(tool), &scope, None);
        self.before(act);
        let res = self.link.raw(act, tool, args);
        let mut counts = RouteCounts::default();
        if let Res::Ok(v) = &res {
            counts.profile_refs = v["references"]["rows"].as_array().map_or(0, Vec::len);
            if let Some(dir) = out {
                let scan = scan_local_links(&tree(dir));
                if !scan.broken.is_empty() {
                    self.link
                        .fail(act, &format!("broken local links: {:?}", scan.broken));
                }
                if tree(dir).is_empty() {
                    self.link
                        .fail(act, &format!("{} holds nothing", dir.display()));
                }
                counts.links_checked = scan.checked;
            }
        }
        a = a.counted(counts);
        let written = out.map(|d| d.to_string_lossy().into_owned());
        self.journal(a, res, |v| match &written {
            Some(d) => (d.clone(), vec![]),
            None => (
                v["references"]["status"]
                    .as_str()
                    .unwrap_or("checked")
                    .to_string(),
                vec![],
            ),
        });
    }

    fn publication(&mut self) {
        // a directory of its own under the process's run directory, kept for the oracle: two runs
        // of one seed in one process (the follower's and the sim's) never share it
        let out = tempfile::Builder::new()
            .prefix(&format!("m3pub-s{}-{}-", self.link.at.seed, self.me()))
            .tempdir_in(run_parent())
            .map(tempfile::TempDir::keep)
            .unwrap_or_else(|e| {
                self.link.fail(
                    "m3_publication_output",
                    &format!("cannot make its directory: {e}"),
                )
            });
        self.report.m3_outputs.push(out.clone());
        // the check of canon while the agents work
        let check = json!({"profile": PROFILE, "scope": "canon"});
        self.publish("m3_publication_check", "rdy_publication_check", check, None);
        // a named scope that holds: canon's newest landing
        let mut landing = None;
        let res = self.fixture(None, "rdy_log", json!({"limit": 1}), |_, v| {
            let rows = v.as_array().or_else(|| v["landings"].as_array());
            landing = rows
                .and_then(|r| r.first())
                .and_then(|r| r["landing"].as_str())
                .map(str::to_string);
        });
        if !matches!(res, Res::Ok(_)) {
            return;
        }
        let landing =
            landing.unwrap_or_else(|| self.link.fail("m3_publication_check", "no landing"));
        let scope = format!("landing:{landing}");
        let check = json!({"profile": PROFILE, "scope": scope});
        self.publish("m3_publication_check", "rdy_publication_check", check, None);
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let jobs: [(&'static str, Value, PathBuf); 3] = [
            (
                "rdy_export",
                json!({"out": s(&out.join("md")), "format": "md", "profile": PROFILE, "scope": scope}),
                out.join("md"),
            ),
            (
                "rdy_export",
                json!({"out": s(&out.join("out.html")), "format": "html", "profile": PROFILE, "scope": scope}),
                out.join("out.html"),
            ),
            (
                "rdy_render",
                json!({"out": s(&out.join("site")), "profile": PROFILE, "scope": scope}),
                out.join("site"),
            ),
        ];
        for (tool, args, at) in jobs {
            self.publish("m3_publication_output", tool, args, Some(&at));
        }
    }

    fn compact(&mut self) {
        let view = self.view("x");
        let claim = self.claim(self.sc.slot);
        let gate = self.link.gate.clone();
        {
            // the removal window: no other act runs from the request to its landing (TASK-1233)
            let _landing = gate.landing_turn();
            let _plan = gate.plan_turn();
            // one copy of the project's log before the request, kept for the oracle: the drained
            // log holds the removed payloads zeroed, so a landing before the removal is audited
            // on the log cut before it with these payloads (the window keeps every landing out
            // of the copy's tail)
            if self.report.pre_removal.is_none() {
                self.report.pre_removal = Some(self.pre_removal_copy());
            }
            // the person asks for the note's removal; the agent records the request
            let args = json!({"targets": [claim], "reason_class": "retraction",
                "reason": "The person asks to remove this note.", "view": view});
            let res = self.fixture(Some(&view), "rdy_redact", args, |en, _| {
                en.block = Some(claim.clone());
            });
            if !matches!(res, Res::Ok(_)) {
                return;
            }
            let Some(pref) = self.propose(&view, true) else {
                return;
            };
            let Some(before) = self.before_answer(&pref) else {
                return;
            };
            let landed = self.relay(
                "m3_blob_remove",
                &pref,
                before,
                &format!("Yes, remove that note: land {pref}."),
            );
            if !landed {
                return;
            }
        }
        // the snapshot before, in the landing turn
        self.snapshot();
        // the real compact, while the agents work
        let act = "m3_compact_real";
        let mut a = self.attempt(act, Via::Tool("rdy_compact"), "canon", None);
        self.before(act);
        let res = self.link.raw(act, "rdy_compact", json!({"dry_run": false}));
        if let Res::Ok(v) = &res {
            let removed = v["removed_bytes"].as_u64().unwrap_or(0);
            if removed == 0 {
                self.link
                    .fail(act, &format!("compact saw no removed payload: {v}"));
            }
            a = a.counted(RouteCounts {
                removals: 1,
                ..RouteCounts::default()
            });
        }
        let res = self.journal(a, res, |v| {
            (format!("removed {}", v["removed_bytes"]), vec![])
        });
        if matches!(res, Res::Ok(_)) {
            // the snapshot after
            self.snapshot();
        }
    }

    /// A copy of the project's log (its store's `segments`, the only part a cut reads; the rest
    /// changes under the live daemon) in a directory of its own under the process's run
    /// directory, kept for the oracle (two runs of one seed in one process never share it).
    fn pre_removal_copy(&self) -> PathBuf {
        let made = tempfile::Builder::new()
            .prefix(&format!("m3pre-s{}-{}-", self.link.at.seed, self.me()))
            .tempdir_in(run_parent())
            .map(tempfile::TempDir::keep);
        let to = made
            .map(|d| d.join("segments"))
            .map_err(|e| e.to_string())
            .and_then(|to| {
                let log = self
                    .link
                    .at
                    .dir
                    .join(".rdy")
                    .join(STORE_DIR)
                    .join("segments");
                copy_tree(&log, &to).map(|()| to).map_err(|e| e.to_string())
            });
        to.unwrap_or_else(|e| {
            self.link.fail(
                "m3_blob_remove",
                &format!("cannot copy the project before the removal: {e}"),
            )
        })
    }

    /// `rdy_compact {dry_run: true}` in the landing turn: a snapshot of what compaction counts.
    fn snapshot(&mut self) {
        let gate = self.link.gate.clone();
        let _turn = gate.landing_turn();
        self.fixture(None, "rdy_compact", json!({"dry_run": true}), |en, v| {
            en.reply = Some(v.clone());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREF: &str = "P-7f3a@2";

    /// A queue item as `rdy_pending` gives it at the largest budget: the text and the fields
    /// the text renders.
    fn item() -> Value {
        let card = format!(
            "{PREF}  Guest mode                                              approve\n\n\
             Summary (agent:planner)\n  Lets a visitor read a shared doc without an account.\n\n\
             Facts (computed)\n  Changes         2 docs, 3 blocks\n\n\
             Options\n  1. Approve {PREF}                              recommended\n     \
             Lands the proposal in canon.\n     Why recommended: no exception.\n  2. Reject\n     \
             Closes it."
        );
        json!({
            "ref": PREF, "kind": "approve", "title": "Guest mode",
            "summary": "Lets a visitor read a shared doc without an account.",
            "options": [
                {"id": "approve", "label": format!("Approve {PREF}"),
                 "consequences": ["Lands the proposal in canon."], "recommended": true,
                 "reason": "no exception."},
                {"id": "reject", "label": "Reject", "consequences": ["Closes it."]},
            ],
            "card": card, "card_hash": "ab12",
        })
    }

    fn queue(i: Value) -> Value {
        json!({"items": [i]})
    }

    #[test]
    fn a_whole_card_is_presented_with_its_labels() {
        let c = card_in(&queue(item()), PREF).expect("the card");
        assert_eq!(c.complete(), Ok(()));
        assert_eq!(c.title, "Guest mode");
        assert!(c.text.starts_with(PREF));
        assert_eq!(c.options, ["approve", "reject"]);
        assert_eq!(c.labels, [format!("Approve {PREF}"), "Reject".to_string()]);
        assert_eq!(c.recommended.as_deref(), Some("approve"));
        assert_eq!(c.item, item());
    }

    #[test]
    fn a_card_with_no_text_is_not_complete() {
        let mut i = item();
        i["card"] = json!("");
        let c = card_in(&queue(i), PREF).expect("the card");
        assert!(c.complete().is_err());
    }

    #[test]
    fn a_card_whose_text_leaves_out_an_option_label_is_not_complete() {
        let mut i = item();
        let text = i["card"]
            .as_str()
            .unwrap()
            .replace("  2. Reject", "  2. Close");
        i["card"] = json!(text);
        let c = card_in(&queue(i), PREF).expect("the card");
        let why = c.complete().expect_err("a label left out");
        assert!(why.contains("Reject"), "{why}");
    }

    #[test]
    fn a_card_of_another_version_or_title_is_not_complete() {
        let mut i = item();
        i["title"] = json!("Host mode");
        let c = card_in(&queue(i), PREF).expect("the card");
        assert!(c.complete().is_err());
    }

    #[test]
    fn a_cut_card_is_not_complete() {
        let mut i = item();
        let text = i["card"].as_str().unwrap().replace(
            "  Changes         2 docs, 3 blocks",
            "  (1 facts cut; rdy_pending with a larger max_tokens shows them)",
        );
        i["card"] = json!(text);
        let c = card_in(&queue(i), PREF).expect("the card");
        assert!(c.complete().is_err());
    }

    #[test]
    fn a_summary_or_consequence_the_text_leaves_out_is_not_complete() {
        let mut i = item();
        i["summary"] = json!("Something the card does not say.");
        assert!(card_in(&queue(i), PREF).unwrap().complete().is_err());
        let mut i = item();
        i["options"][1]["consequences"] = json!(["Deletes everything."]);
        assert!(card_in(&queue(i), PREF).unwrap().complete().is_err());
    }

    #[test]
    fn a_removal_card_must_show_each_request_and_its_warnings() {
        let mut i = item();
        i["removal"] = json!([{"request": "aa"}]);
        let c = card_in(&queue(i.clone()), PREF).unwrap();
        assert!(c.complete().is_err(), "no removal section");
        let text = format!(
            "{}\n\n{REMOVAL_SECTION}\n  {}\n  {}\n  {}",
            i["card"].as_str().unwrap(),
            rdy_engine::W_REDACTION_COPIES_MESSAGE,
            rdy_engine::W_REDACTION_QUOTES_MESSAGE,
            rdy_engine::W_REDACTION_REUSE_MESSAGE
        );
        i["card"] = json!(text);
        assert_eq!(card_in(&queue(i), PREF).unwrap().complete(), Ok(()));
    }

    #[test]
    fn the_person_chooses_by_the_numbered_label_the_card_shows() {
        let c = card_in(&queue(item()), PREF).unwrap();
        assert_eq!(choose(&c, APPROVE).as_deref(), Some("approve"));
        assert_eq!(choose(&c, "Reject").as_deref(), Some("reject"));
        assert_eq!(choose(&c, "Later"), None);
        // an id whose label the text does not number is never chosen
        let mut i = item();
        let text = i["card"]
            .as_str()
            .unwrap()
            .replace("  1. Approve", "  1. Apprvoe");
        i["card"] = json!(text);
        assert_eq!(choose(&card_in(&queue(i), PREF).unwrap(), APPROVE), None);
        // the number on the card, not the id's place in a list the person never saw
        let mut i = item();
        let opts = i["options"].as_array().unwrap().clone();
        i["options"] = json!([opts[1], opts[0]]);
        assert_eq!(choose(&card_in(&queue(i), PREF).unwrap(), APPROVE), None);
    }
}
