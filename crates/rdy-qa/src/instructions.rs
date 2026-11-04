//! Gate helpers for agent instructions (ain4): projects whose canon holds rulesets, and an
//! isolated person's world (a temporary home and user config) whose `rdy` processes and
//! daemons never read the real user config or the real meta project.
use crate::daemon::{human_command, short_tempdir};
use crate::fixtures::{land, write, writer};
use crate::mcp::McpClient;
use rdy_engine::instructions_config::{PathEnv, Platform};
use rdy_engine::{DecisionInput, Rdy};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Mutex;

/// The reminder line, as the plan gives it.
pub const REMINDER: &str = "Call rdy_instructions at the start of every task.";

/// One ruleset to author: a doc with an optional lead and its instructions, each a list of
/// selectors and a body.
pub struct Set<'a> {
    pub name: &'a str,
    pub title: &'a str,
    pub required: bool,
    pub lead: Option<&'a str>,
    pub rules: &'a [(&'a [&'a str], &'a str)],
}

/// What [`author`] landed: the reason decision, and each set's doc id and instruction ids.
pub struct Authored {
    pub decision: String,
    pub statement: String,
    pub docs: Vec<String>,
    pub rules: Vec<Vec<String>>,
}

/// Lands in the project at `dir` (no process may hold its store) a decision with `statement`
/// and every set in `sets`, each instruction with that decision as its reason.
pub fn author(dir: &Path, statement: &str, sets: &[Set]) -> Authored {
    let mut r = Rdy::open(dir).expect("open the project to author rules");
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "decisions", "title": "Decisions", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}}
        ]),
    )
    .expect("the decisions doc");
    let decision = r
        .decide(
            &writer(),
            "seed",
            DecisionInput {
                parent: w.created["$s"].clone(),
                statement: statement.into(),
                ..Default::default()
            },
        )
        .expect("the reason")
        .created["$decision"]
        .clone();
    let mut docs = vec![];
    let mut rules = vec![];
    for set in sets {
        let mut fields = json!({"name": set.name, "title": set.title, "doc_type": "page"});
        if set.required {
            fields["required"] = json!(true);
        }
        let mut ops = vec![json!({"op": "create", "type": "doc", "id": "$d", "fields": fields})];
        if let Some(lead) = set.lead {
            ops.push(json!({"op": "create", "type": "prose", "parent": "$d", "body": lead}));
        }
        for (i, (sels, body)) in set.rules.iter().enumerate() {
            ops.push(
                json!({"op": "create", "type": "instruction", "id": format!("$i{i}"),
                "parent": "$d", "fields": {"applies_to": sels}, "body": body,
                "links": {"decided_by": [{"target": decision}]}}),
            );
        }
        let w = write(&mut r, "seed", Value::Array(ops))
            .unwrap_or_else(|e| panic!("ruleset {}: {e}", set.name));
        docs.push(w.created["$d"].clone());
        rules.push(
            (0..set.rules.len())
                .map(|i| w.created[&format!("$i{i}")].clone())
                .collect(),
        );
    }
    land(&mut r, "seed").expect("land the rules");
    r.close().expect("close the project");
    Authored {
        decision,
        statement: statement.into(),
        docs,
        rules,
    }
}

/// A person's isolated world: `HOME` and `XDG_CONFIG_HOME` are temporary, the user config names
/// the human `human:qa`, and every project made here has its daemon stopped at drop.
pub struct World {
    pub home: tempfile::TempDir,
    cfg: tempfile::TempDir,
    projects: Mutex<Vec<PathBuf>>,
    /// The project directories [`World::project`] made: removed only after their daemons stop.
    dirs: Mutex<Vec<tempfile::TempDir>>,
}

/// A project directory of a [`World`], which removes it once its daemon stopped.
#[derive(Clone, Debug)]
pub struct Dir(PathBuf);

impl Dir {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Default for World {
    fn default() -> World {
        World::new()
    }
}

impl World {
    pub fn new() -> World {
        let w = World {
            home: short_tempdir(),
            cfg: short_tempdir(),
            projects: Mutex::new(vec![]),
            dirs: Mutex::new(vec![]),
        };
        w.config("");
        w
    }

    /// The user config: the human, then `extra` as written.
    pub fn config(&self, extra: &str) {
        std::fs::create_dir_all(self.cfg.path().join("rdy")).expect("config dir");
        std::fs::write(
            self.cfg.path().join("rdy/config.yaml"),
            format!("actor: human:qa\n{extra}"),
        )
        .expect("user config");
    }

    /// The user config with `instructions.person` set to `path`.
    pub fn person(&self, path: &Path) {
        self.config(&format!("instructions:\n  person: {}\n", path.display()));
    }

    /// The default meta project's place: `<home>/.rdy/meta`.
    pub fn default_meta(&self) -> PathBuf {
        self.home.path().join(".rdy").join("meta")
    }

    /// The environment of every process of this world.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "HOME".to_string(),
                self.home.path().to_string_lossy().into_owned(),
            ),
            (
                "XDG_CONFIG_HOME".to_string(),
                self.cfg.path().to_string_lossy().into_owned(),
            ),
        ]
    }

    /// The same environment as data, for an in-process reader.
    pub fn path_env(&self) -> (Platform, PathEnv) {
        (
            Platform::HOST,
            PathEnv {
                xdg_config_home: Some(self.cfg.path().into()),
                home: Some(self.home.path().into()),
                ..PathEnv::default()
            },
        )
    }

    /// A fresh `rdy-standard` project at `dir` (made if missing), with a 60 s idle timeout; its
    /// daemon is stopped at drop.
    pub fn project_at(&self, dir: &Path) {
        std::fs::create_dir_all(dir).expect("project dir");
        drop(Rdy::init(dir, "rdy-standard").expect("init the project"));
        let cfg = dir.join(".rdy.yaml");
        let mut text = std::fs::read_to_string(&cfg).expect("project config");
        text.push_str("serve:\n  idle_timeout: 60\n");
        std::fs::write(&cfg, text).expect("project config");
        self.projects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(dir.to_path_buf());
    }

    /// A fresh project in a temporary directory ([`World::project_at`]).
    pub fn project(&self) -> Dir {
        let dir = short_tempdir();
        self.project_at(dir.path());
        let path = dir.path().to_path_buf();
        self.dirs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(dir);
        Dir(path)
    }

    /// `rdy --project <dir> <args>` as the human of this world.
    pub fn rdy(&self, dir: &Path, args: &[&str]) -> Output {
        human_command(dir, &self.env(), args)
            .output()
            .expect("run rdy")
    }

    /// The JSON of `rdy --project <dir> --json <args>`: stdout on success, else stderr.
    pub fn json(&self, dir: &Path, args: &[&str]) -> (bool, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.rdy(dir, &all);
        let ok = out.status.success();
        let text = String::from_utf8_lossy(if ok { &out.stdout } else { &out.stderr }).into_owned();
        let v = serde_json::from_str(text.trim())
            .or_else(|_| serde_json::from_str(text.trim().lines().last().unwrap_or("")))
            .unwrap_or(Value::String(text));
        (ok, v)
    }

    /// `rdy mcp --project <dir>` in this world: the proxy to the project's daemon.
    pub fn mcp(&self, dir: &Path) -> McpClient {
        McpClient::start_proxy_at(dir, &self.env())
    }

    /// The pid of the daemon serving `dir`, while one runs.
    pub fn daemon_pid(&self, dir: &Path) -> Option<u32> {
        let run = rdy_daemon::RunDir::of_project(dir);
        let pid: u32 = std::fs::read_to_string(run.pid_file())
            .ok()?
            .trim()
            .parse()
            .ok()?;
        crate::platform::process_running(pid).then_some(pid)
    }

    /// Stops the daemon of `dir` (`rdy serve --stop`).
    pub fn stop(&self, dir: &Path) {
        // never a panic: a drop may run while a test unwinds
        if let Err(e) = human_command(dir, &self.env(), &["serve", "--stop"]).output() {
            eprintln!("rdy serve --stop did not run in {}: {e}", dir.display());
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let dirs = std::mem::take(&mut *self.projects.lock().unwrap_or_else(|p| p.into_inner()));
        for d in dirs {
            if self.daemon_pid(&d).is_some() {
                self.stop(&d);
            }
        }
    }
}
