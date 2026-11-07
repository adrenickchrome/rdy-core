//! The host's reader of the person layer (ain4, D-YQPB6, D-A8N0E): the meta project's
//! contribution, read through that project's own daemon with the private form of the ctl
//! `instructions` op. The reader never opens a store its daemon holds, never asks for a second
//! source, and never claims the human: it asks as the caller's actor and run.
//!
//! A daemon's reader always asks meta's daemon, starting it as any client would
//! ([`connect_or_start`]). A direct run's reader (the CLI's `--no-daemon`) opens an unused meta
//! store itself, reads its canon once and closes it; when a process holds that store, it asks
//! meta's daemon instead. No step retries a store lock past the existing bounds, and none stops
//! a healthy daemon.
//!
//! The same host writes a rule to that project (FW-MH.rules, D-ZSRBN), with the private form of the
//! ctl `rule` op. One connection stays open across the draft, the signing and the landing, and its
//! close leaves the daemon on its idle policy. The daemon asks for signatures on that connection,
//! and [`ForwardSigner`] answers with the signer of the caller's call: never this process's own
//! environment. A reply that is lost is `E_DAEMON_GONE`, which says a write may have committed, and
//! nothing here retries it: only the caller's explicit repeat reads the receipt.
use crate::client::{StartOptions, connect_or_start};
use crate::layout::RunDir;
use crate::session::Proto;
use crate::signing_transport::{CallerSigner, InterruptSource, never_interrupted, sigint};
use crate::spawn::{Spawned, serve_command, spawn_serve};
use rdy_engine::engine::Local;
use rdy_engine::instructions::{
    self, Caller, Contribution, ForwardedRule, Layer, LoadRequest, PersonHost,
};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::rules::approval::run_rule;
use rdy_engine::rules::{Destination, RuleOrigin};
use rdy_engine::signing_crypto::{AgentIdentity, CallCancel, SigningAgent, SigningCall};
use rdy_engine::{E_HELD_BY_READER, E_INTERNAL, E_LOCKED, NoAgentCause, Rdy, RdyError, no_agent};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

/// How the reader reaches the meta project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Through meta's daemon only: the reader runs inside a daemon.
    Daemon,
    /// An unused meta store is opened in this process; a held one is read through its daemon.
    Direct,
}

/// The reader of the person layer.
pub struct PersonReader {
    mode: Mode,
    /// The version this process says in its hello to meta's daemon.
    version: String,
    /// The environment that places the user config; `None` reads this process's.
    env: Option<(Platform, PathEnv)>,
    /// What starts meta's daemon; `None` is this binary ([`start_options`]).
    starter: Option<Starter>,
}

/// Starts a daemon for the project at its first argument, in its run directory: a test seam.
pub type Starter = Arc<dyn Fn(&Path, &RunDir) -> std::io::Result<Spawned> + Send + Sync>;

impl PersonReader {
    /// A reader inside a daemon of `version`.
    pub fn daemon(version: impl Into<String>) -> PersonReader {
        PersonReader {
            mode: Mode::Daemon,
            version: version.into(),
            env: None,
            starter: None,
        }
    }

    /// A reader for a direct run of `version`.
    pub fn direct(version: impl Into<String>) -> PersonReader {
        PersonReader {
            mode: Mode::Direct,
            version: version.into(),
            env: None,
            starter: None,
        }
    }

    /// This reader with the user config placed by `env` on `platform`, not by this process's
    /// environment: a test seam.
    pub fn with_env(mut self, platform: Platform, env: PathEnv) -> PersonReader {
        self.env = Some((platform, env));
        self
    }

    /// This reader with `starter` in place of this binary as what starts meta's daemon: a test
    /// seam, since a test's own binary is no `rdy`.
    #[doc(hidden)]
    pub fn with_starter(mut self, starter: Starter) -> PersonReader {
        self.starter = Some(starter);
        self
    }

    /// The contribution of the unused store at `meta`, or `None` when a process holds it.
    fn open_unused(meta: &Path, load: &LoadRequest) -> Result<Option<Contribution>, RdyError> {
        let rdy = match Rdy::open(meta) {
            Ok(r) => r,
            Err(e) if e.code == E_LOCKED || e.code == E_HELD_BY_READER => return Ok(None),
            Err(e) => return Err(e),
        };
        let part = instructions::collect(&rdy.read(), Layer::Person, load);
        let closed = rdy.close();
        let part = part?;
        closed?;
        Ok(Some(part))
    }

    /// The contribution of `meta` as its daemon reads it, asked as `caller`.
    fn ask_daemon(
        &self,
        meta: &Path,
        load: &LoadRequest,
        caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let args = json!({"load": load.given(), "part": true});
        let session = json!({"actor": caller.actor, "run": caller.run});
        let asked = self.ask(meta, "read", session, "instructions", args, None)?;
        let part = asked.get("contribution").cloned().unwrap_or(Value::Null);
        serde_json::from_value(part).map_err(|e| {
            RdyError::new(
                E_INTERNAL,
                format!("the meta project's daemon sent an unreadable contribution: {e}"),
                "report this as a bug in rdy",
            )
        })
    }

    /// One ctl request to `meta`'s daemon, in a session of its own, starting that daemon when none
    /// runs. The ask runs on a runtime of its own, on a thread of its own, so no caller's runtime or
    /// lock is held across it. `act` names it in the refusals of its runtime. `signer` answers the
    /// daemon's signing callbacks, and what interrupts an ask of it.
    fn ask(
        &self,
        meta: &Path,
        act: &str,
        session: Value,
        op: &'static str,
        args: Value,
        signer: Option<(Arc<dyn CallerSigner>, InterruptSource)>,
    ) -> Result<Value, RdyError> {
        let opts = self.start_options(meta);
        let run = RunDir::of_project(meta);
        std::thread::scope(|s| {
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        RdyError::new(
                            rdy_engine::E_IO,
                            format!("cannot start a runtime to {act} the person layer: {e}"),
                            "retry",
                        )
                    })?;
                rt.block_on(async move {
                    let mut c = connect_or_start(&run, opts)
                        .await
                        .map_err(crate::maintenance::rdy_error)?;
                    if let Some((agent, interrupt)) = signer {
                        c = c.with_agent(agent).with_interrupt(interrupt);
                    }
                    c.ctl("session", session)
                        .await
                        .map_err(crate::maintenance::rdy_error)?;
                    let v = c
                        .ctl(op, args)
                        .await
                        .map_err(crate::maintenance::rdy_error)?;
                    c.close();
                    Ok::<Value, RdyError>(v)
                })
            })
            .join()
            .unwrap_or_else(|_| {
                Err(RdyError::new(
                    E_INTERNAL,
                    format!("the person layer's {act} panicked"),
                    "this is a bug in rdy; retry",
                ))
            })
        })
    }

    /// `rule` through `meta`'s daemon: its own writer, its own configured human, and the caller's
    /// actor and run, as the session's role was checked before this ([`rdy_engine::rules::route_rule`]).
    /// A daemon's reader takes no signal: its caller's own process does.
    fn forward_rule(&self, meta: &Path, rule: &ForwardedRule<'_>) -> Result<Value, RdyError> {
        let mut args = rule.request.to_json();
        args["part"] = json!(true);
        if let Some(by) = rule.relayed_by {
            args["relayed_by"] = json!(by);
        }
        let session = json!({"actor": rule.session.actor, "run": rule.session.run});
        let interrupt = match self.mode {
            Mode::Daemon => never_interrupted(),
            Mode::Direct => sigint(),
        };
        let signer: Arc<dyn CallerSigner> = Arc::new(ForwardSigner {
            agent: Arc::clone(&rule.agent),
        });
        self.ask(
            meta,
            "write",
            session,
            "rule",
            args,
            Some((signer, interrupt)),
        )
    }

    /// `rule` in the unused store at `meta`, opened here once and closed: `None` when a process holds
    /// it, with the refusal that says so. A close that fails after the rule's own step is a warning:
    /// that step stands.
    fn run_unused(
        &self,
        meta: &Path,
        rule: &ForwardedRule<'_>,
    ) -> Result<Result<Value, RdyError>, RdyError> {
        let person = self.person_actor(meta)?;
        let local = match Local::open(meta) {
            Ok(l) => l,
            Err(e) if e.code == E_LOCKED || e.code == E_HELD_BY_READER => return Ok(Err(e)),
            Err(e) => return Err(e),
        };
        let origin = RuleOrigin {
            session: rule.session,
            relayed_by: rule.relayed_by,
        };
        let call = origin.call(person, Destination::Meta);
        let out = run_rule(&local, &*rule.agent, &call, rule.request).map(|r| r.to_json());
        let closed = local.close();
        let mut v = out?;
        if let Err(e) = closed {
            let warning = json!({
                "severity": "warning",
                "code": e.code,
                "block": null,
                "message": e.message,
                "fix": e.fix,
            });
            let list = match v.get("landing") {
                Some(_) => &mut v["landing"]["warnings"],
                None => &mut v["warnings"],
            };
            match list.as_array_mut() {
                Some(l) => l.push(warning),
                None => *list = json!([warning]),
            }
        }
        Ok(Ok(v))
    }
}

/// The caller's signer for one call, on the connection that call forwards to meta's daemon: each
/// ask of the daemon goes to it. The link's cancel reaches it through its own call
/// ([`SigningAgent::for_call`]), and an answer that comes after that cancel is dropped.
struct ForwardSigner {
    agent: Arc<dyn SigningAgent>,
}

impl ForwardSigner {
    fn ask<T>(
        &self,
        cancel: &CallCancel,
        f: impl FnOnce(&dyn SigningAgent) -> Result<T, RdyError>,
    ) -> Result<T, RdyError> {
        let bound = Arc::new(CallCancel::new());
        let hook = {
            let bound = Arc::clone(&bound);
            cancel.on_cancel(Box::new(move || bound.cancel()))
        };
        let Some(hook) = hook else {
            return Err(no_agent(NoAgentCause::Cancelled));
        };
        // the call's own signer already names its request; a bare agent asks outside any request
        let agent = self.agent.for_call(&SigningCall {
            request: Value::Null,
            cancel: bound,
        });
        let out = f(agent.as_deref().unwrap_or(&*self.agent));
        cancel.forget(hook);
        if cancel.is_cancelled() {
            return Err(no_agent(NoAgentCause::Cancelled));
        }
        out
    }
}

impl CallerSigner for ForwardSigner {
    fn identities_until(&self, cancel: &CallCancel) -> Result<Vec<AgentIdentity>, RdyError> {
        self.ask(cancel, |a| a.identities())
    }

    fn sign_until(
        &self,
        public_key: &[u8],
        payload: &[u8],
        cancel: &CallCancel,
    ) -> Result<Vec<u8>, RdyError> {
        self.ask(cancel, |a| a.sign(public_key, payload))
    }
}

impl PersonReader {
    /// The options of a ctl connection to `meta`'s daemon: this binary spawns it when none runs,
    /// as every client does (`rdy serve --detach --spawned`), with meta's own start timeout.
    fn start_options(&self, meta: &Path) -> StartOptions {
        let project = meta.to_path_buf();
        let starter = self.starter.clone();
        let mut opts = StartOptions::new(
            self.version.clone(),
            Proto::Ctl,
            meta.to_path_buf(),
            Box::new(move |run| match &starter {
                Some(start) => start(&project, run),
                None => spawn_serve(&mut serve_command(
                    &std::env::current_exe()?,
                    &project,
                    run,
                )?),
            }),
        );
        opts.start_timeout = rdy_engine::serve_config(meta).start_timeout;
        opts
    }
}

impl PersonHost for PersonReader {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        match &self.env {
            Some((platform, env)) => resolve_person(*platform, env),
            None => rdy_engine::instructions_config::resolve_person_from_env(),
        }
    }

    fn person_actor(&self, project: &Path) -> Result<String, RdyError> {
        match &self.env {
            Some((platform, env)) => {
                let file = rdy_engine::instructions_config::user_config_path(*platform, env);
                rdy_engine::human_actor_for(project, file.as_deref(), None)
            }
            None => rdy_engine::human_actor_for_env(project),
        }
    }

    /// The meta project at `meta`: a directory that is no project is its `E_NO_PROJECT`, and
    /// every refusal of its open or its daemon reaches the caller unchanged.
    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        rdy_engine::config_levels::require_project(meta)?;
        if self.mode == Mode::Direct
            && let Some(part) = PersonReader::open_unused(meta, load)?
        {
            return Ok(part);
        }
        self.ask_daemon(meta, load, caller)
    }

    /// The meta project at `meta`, as [`PersonHost::fetch`] finds it. A direct run uses an unused
    /// store itself; a held one goes to its daemon. When no daemon can serve a held store, the
    /// lock's refusal is the one that says why.
    fn write_rule(&self, meta: &Path, rule: &ForwardedRule<'_>) -> Result<Value, RdyError> {
        rdy_engine::config_levels::require_project(meta)?;
        if self.mode == Mode::Direct {
            let held = match self.run_unused(meta, rule)? {
                Ok(v) => return Ok(v),
                Err(held) => held,
            };
            return match self.forward_rule(meta, rule) {
                Err(e) if e.code == crate::error::E_DAEMON_START => Err(held),
                out => out,
            };
        }
        self.forward_rule(meta, rule)
    }
}
