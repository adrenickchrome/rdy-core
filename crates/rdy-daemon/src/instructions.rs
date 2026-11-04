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
use crate::client::{StartOptions, connect_or_start};
use crate::layout::RunDir;
use crate::session::Proto;
use crate::spawn::{serve_command, spawn_serve};
use rdy_engine::instructions::{self, Caller, Contribution, Layer, LoadRequest, PersonHost};
use rdy_engine::instructions_config::{PathEnv, PersonSource, Platform, resolve_person};
use rdy_engine::{E_HELD_BY_READER, E_INTERNAL, E_LOCKED, Rdy, RdyError};
use serde_json::{Value, json};
use std::path::Path;

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
}

impl PersonReader {
    /// A reader inside a daemon of `version`.
    pub fn daemon(version: impl Into<String>) -> PersonReader {
        PersonReader {
            mode: Mode::Daemon,
            version: version.into(),
            env: None,
        }
    }

    /// A reader for a direct run of `version`.
    pub fn direct(version: impl Into<String>) -> PersonReader {
        PersonReader {
            mode: Mode::Direct,
            version: version.into(),
            env: None,
        }
    }

    /// This reader with the user config placed by `env` on `platform`, not by this process's
    /// environment: a test seam.
    pub fn with_env(mut self, platform: Platform, env: PathEnv) -> PersonReader {
        self.env = Some((platform, env));
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

    /// The contribution of `meta` as its daemon reads it, asked as `caller`. The ask runs on a
    /// runtime of its own, on a thread of its own, so no caller's runtime or lock is held
    /// across it.
    fn ask_daemon(
        &self,
        meta: &Path,
        load: &LoadRequest,
        caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let args = json!({"load": load.given(), "part": true});
        let session = json!({"actor": caller.actor, "run": caller.run});
        let opts = start_options(meta, &self.version);
        let run = RunDir::of_project(meta);
        let asked = std::thread::scope(|s| {
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| {
                        RdyError::new(
                            rdy_engine::E_IO,
                            format!("cannot start a runtime to read the person layer: {e}"),
                            "retry",
                        )
                    })?;
                rt.block_on(async move {
                    let mut c = connect_or_start(&run, opts)
                        .await
                        .map_err(crate::maintenance::rdy_error)?;
                    c.ctl("session", session)
                        .await
                        .map_err(crate::maintenance::rdy_error)?;
                    let v = c
                        .ctl("instructions", args)
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
                    "the person layer's read panicked",
                    "this is a bug in rdy; retry",
                ))
            })
        })?;
        let part = asked.get("contribution").cloned().unwrap_or(Value::Null);
        serde_json::from_value(part).map_err(|e| {
            RdyError::new(
                E_INTERNAL,
                format!("the meta project's daemon sent an unreadable contribution: {e}"),
                "report this as a bug in rdy",
            )
        })
    }
}

/// The options of a ctl connection to `meta`'s daemon: this binary spawns it when none runs, as
/// every client does (`rdy serve --detach --spawned`), with meta's own start timeout.
fn start_options(meta: &Path, version: &str) -> StartOptions {
    let project = meta.to_path_buf();
    let mut opts = StartOptions::new(
        version.to_string(),
        Proto::Ctl,
        meta.to_path_buf(),
        Box::new(move |run| {
            spawn_serve(&mut serve_command(
                &std::env::current_exe()?,
                &project,
                run,
            )?)
        }),
    );
    opts.start_timeout = rdy_engine::serve_config(meta).start_timeout;
    opts
}

impl PersonHost for PersonReader {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        match &self.env {
            Some((platform, env)) => resolve_person(*platform, env),
            None => rdy_engine::instructions_config::resolve_person_from_env(),
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
}
