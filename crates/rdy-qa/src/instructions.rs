//! Gate helpers for agent instructions (ain4): projects whose canon holds rulesets, and an
//! isolated person's world (a temporary home and user config, or no home at all) whose `rdy`
//! processes and daemons never read the real user config or the real meta project.
use crate::daemon::{human_command, short_tempdir};
use crate::fixtures::{land, write, writer};
use crate::mcp::McpClient;
use crate::platform::ProcessGroup;
use rdy_engine::instructions_config::{
    PathEnv, PersonPath, PersonSource, Platform, resolve_person,
};
use rdy_engine::{DecisionInput, Rdy};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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

/// Every variable that places a person's home, config or meta project (`PathEnv`). A process of a
/// [`World`] gets each one from the world or has it removed, so it inherits none.
const PERSON_VARS: [&str; 6] = [
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "XDG_CONFIG_HOME",
];

/// The person variables of a world, as data. With no `home`, only the config place remains. The
/// drive form exists only where the home has a drive, which is `platform`'s text rule.
pub(crate) fn person_env(platform: Platform, home: Option<&Path>, cfg: &Path) -> PathEnv {
    let drive = home.and_then(|h| {
        let text = h.to_str()?;
        let is_drive = platform == Platform::Windows
            && text.as_bytes().first()?.is_ascii_alphabetic()
            && text.as_bytes().get(1) == Some(&b':');
        is_drive.then(|| text.split_at(2))
    });
    PathEnv {
        xdg_config_home: Some(cfg.into()),
        home: home.map(Into::into),
        user_profile: home.map(Into::into),
        home_drive: drive.map(|(d, _)| d.into()),
        home_path: drive.map(|(_, p)| p.into()),
        app_data: home.map(|h| h.join("AppData").into()),
        // The account name places nothing: a world's config names the human.
        ..PathEnv::default()
    }
}

/// Each person variable with its value in `vars`, in [`PERSON_VARS`] order; `None` where the
/// variable is not set.
pub(crate) fn person_pairs(vars: &PathEnv) -> [(&'static str, Option<std::ffi::OsString>); 6] {
    [
        ("HOME", vars.home.clone()),
        ("USERPROFILE", vars.user_profile.clone()),
        ("HOMEDRIVE", vars.home_drive.clone()),
        ("HOMEPATH", vars.home_path.clone()),
        ("APPDATA", vars.app_data.clone()),
        ("XDG_CONFIG_HOME", vars.xdg_config_home.clone()),
    ]
}

/// A person's isolated world: every home and config variable is temporary or removed, the user
/// config names the human `human:qa`, and every project made here, and the meta project a process
/// may have made, has its daemon stopped at drop.
pub struct World {
    pub home: tempfile::TempDir,
    cfg: tempfile::TempDir,
    /// No home at all: the "no home directory" case, with the config place alone.
    no_home: bool,
    projects: Mutex<Vec<PathBuf>>,
    /// The project directories [`World::project`] made: removed only after their daemons stop.
    dirs: Mutex<Vec<tempfile::TempDir>>,
    /// Every command the world runs, and every daemon those commands start: on Windows one Job
    /// Object, which ends them when the world drops; on Unix each command leads a process group.
    group: ProcessGroup,
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
        World::with_home(true)
    }

    /// A world whose processes have no home variable at all.
    pub fn without_home() -> World {
        World::with_home(false)
    }

    fn with_home(home: bool) -> World {
        let w = World {
            home: short_tempdir(),
            cfg: short_tempdir(),
            no_home: !home,
            projects: Mutex::new(vec![]),
            dirs: Mutex::new(vec![]),
            group: ProcessGroup::new().expect("the world's process group"),
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

    /// The user config with `instructions.person` set to `path`, as one YAML string.
    pub fn person(&self, path: &Path) {
        let value = serde_yaml_ng::to_string(&path.to_string_lossy()).expect("a YAML string");
        self.config(&format!("instructions:\n  person: {value}"));
    }

    /// The directory `XDG_CONFIG_HOME` names.
    pub fn config_dir(&self) -> PathBuf {
        self.cfg.path().to_path_buf()
    }

    /// The default meta project's place: `<home>/.rdy/meta`.
    pub fn default_meta(&self) -> PathBuf {
        self.home.path().join(".rdy").join("meta")
    }

    /// The person variables of this world, from its own temporary roots alone.
    fn person_vars(&self) -> PathEnv {
        let home = (!self.no_home).then(|| self.home.path());
        person_env(Platform::HOST, home, self.cfg.path())
    }

    /// The environment of every process of this world. [`World::removed_env`] names the rest.
    pub fn env(&self) -> Vec<(String, String)> {
        let pairs = person_pairs(&self.person_vars());
        pairs
            .into_iter()
            .filter_map(|(k, v)| Some((k.to_string(), v?.to_string_lossy().into_owned())))
            .collect()
    }

    /// The person variables this world leaves unset: a process must not inherit them.
    pub fn removed_env(&self) -> Vec<&'static str> {
        let set = self.env();
        PERSON_VARS
            .into_iter()
            .filter(|k| !set.iter().any(|(name, _)| name == k))
            .collect()
    }

    /// The same environment as data, for an in-process reader.
    pub fn path_env(&self) -> (Platform, PathEnv) {
        (Platform::HOST, self.person_vars())
    }

    /// Every project whose daemon this world stops at drop: those it made, the default meta
    /// project, and the meta project the user config moves. A process may create either of the
    /// last two, and no later step tells the world.
    pub fn teardown_paths(&self) -> Vec<PathBuf> {
        let mut all = self
            .projects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if !self.no_home {
            all.push(self.default_meta());
        }
        let (platform, env) = self.path_env();
        if let Ok(PersonSource {
            path: PersonPath::Override(p),
            ..
        }) = resolve_person(platform, &env)
        {
            all.push(p);
        }
        let mut seen = BTreeSet::new();
        all.retain(|p| seen.insert(p.clone()));
        all
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

    /// Gives `cmd` this world's person, over anything it holds already: the person variables
    /// are the world's or removed, `USERNAME` is the fixture's and `SSH_AUTH_SOCK` is removed.
    pub fn isolate<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        cmd.envs(self.env());
        for name in self.removed_env() {
            cmd.env_remove(name);
        }
        crate::fixtures::isolate_account(cmd)
    }

    /// The command `rdy --project <dir> <args>` as the human of this world.
    pub fn command(&self, dir: &Path, args: &[&str]) -> Command {
        let mut cmd = human_command(dir, &self.env(), args);
        self.isolate(&mut cmd);
        cmd
    }

    /// Runs `cmd` in this world's process group, with its output captured. A daemon the command
    /// starts stays in the group after the command ends, and ends with the world.
    pub fn run(&self, cmd: &mut Command) -> std::io::Result<Output> {
        self.group.output(cmd)
    }

    /// `rdy --project <dir> <args>` as the human of this world.
    pub fn rdy(&self, dir: &Path, args: &[&str]) -> Output {
        self.run(&mut self.command(dir, args)).expect("run rdy")
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
        let mut cmd = if self.no_home {
            let mut cmd = crate::fixtures::rdy_command();
            cmd.args(["mcp", "--project"])
                .arg(dir)
                .env_remove("RDY_ACTOR")
                .env_remove("RDY_RUN")
                .env_remove("RDY_NO_DAEMON")
                .stderr(Stdio::piped());
            cmd
        } else {
            McpClient::proxy_command(dir, &self.env())
        };
        self.isolate(&mut cmd);
        McpClient::launch_in(cmd, self.no_home, &self.group)
    }

    /// The pid of the daemon serving `dir`, while one runs.
    pub fn daemon_pid(&self, dir: &Path) -> Option<u32> {
        let run = rdy_daemon::RunDir::of_project(dir);
        let bytes = std::fs::read(run.pid_file()).ok()?;
        let pid = rdy_daemon::sys::parse_pid_record(&bytes)?.pid();
        crate::platform::process_running(pid).then_some(pid)
    }

    /// Stops the daemon of `dir` (`rdy serve --stop`).
    pub fn stop(&self, dir: &Path) {
        // never a panic: a drop may run while a test unwinds
        if let Err(e) = self.run(&mut self.command(dir, &["serve", "--stop"])) {
            eprintln!("rdy serve --stop did not run in {}: {e}", dir.display());
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        for d in self.teardown_paths() {
            if self.daemon_pid(&d).is_some() {
                self.stop(&d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_engine::instructions_config::{default_meta_path, user_config_path};

    fn text(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn agi1_world_isolates_all_person_paths() {
        let w = World::new();
        let (home, cfg) = (w.home.path().to_path_buf(), w.config_dir());
        let vars = w.env();
        let value = |k: &str| {
            vars.iter()
                .find(|(name, _)| name == k)
                .map(|(_, v)| v.clone())
        };

        // The process environment: every variable that places a person path, below a temp root.
        assert_eq!(value("HOME"), Some(text(&home)));
        assert_eq!(value("USERPROFILE"), Some(text(&home)));
        assert_eq!(value("XDG_CONFIG_HOME"), Some(text(&cfg)));
        assert!(Path::new(&value("APPDATA").unwrap()).starts_with(&home));
        for (name, v) in &vars {
            assert!(PERSON_VARS.contains(&name.as_str()), "{name}");
            if name != "HOMEDRIVE" && name != "HOMEPATH" {
                assert!(
                    Path::new(v).starts_with(&home) || Path::new(v).starts_with(&cfg),
                    "{name}={v}"
                );
            }
        }
        for name in w.removed_env() {
            assert!(value(name).is_none(), "{name} is both set and removed");
        }
        let mut all: Vec<&str> = vars.iter().map(|(n, _)| n.as_str()).collect();
        all.extend(w.removed_env());
        all.sort_unstable();
        let mut closed = PERSON_VARS.to_vec();
        closed.sort_unstable();
        assert_eq!(
            all, closed,
            "each variable is set or removed, none inherited"
        );

        // This host: the default and the user config resolve below the temp roots.
        let (platform, env) = w.path_env();
        assert_eq!(default_meta_path(platform, &env), Some(w.default_meta()));
        assert!(w.default_meta().starts_with(&home));
        assert!(user_config_path(platform, &env).unwrap().starts_with(&cfg));
        match resolve_person(platform, &env).unwrap().path {
            PersonPath::Default(p) => assert_eq!(p, w.default_meta()),
            other => panic!("{other:?}"),
        }

        // A configured path is one YAML string, whatever it holds, and stays where it says.
        let other = short_tempdir();
        let tricky = other.path().join("a: b #c 'd\"");
        w.person(&tricky);
        let source = resolve_person(platform, &env).unwrap();
        assert_eq!(source.path, PersonPath::Override(tricky.clone()));
        assert!(source.warnings.is_empty(), "{:?}", source.warnings);

        // Both platform families, as data: a Windows home needs its drive form to stay below too.
        for (platform, home, cfg) in [
            (Platform::Unix, "/tmp/qa/home", "/tmp/qa/cfg"),
            (Platform::Windows, r"C:\qa\home", r"C:\qa\cfg"),
        ] {
            let env = person_env(platform, Some(Path::new(home)), Path::new(cfg));
            let meta = default_meta_path(platform, &env).unwrap();
            assert!(text(&meta).starts_with(home), "{meta:?}");
            let config = user_config_path(platform, &env).unwrap();
            assert!(text(&config).starts_with(cfg), "{config:?}");
            let no_profile = PathEnv {
                user_profile: None,
                ..env.clone()
            };
            let meta = default_meta_path(platform, &no_profile);
            if platform == Platform::Windows {
                assert!(text(&meta.unwrap()).starts_with(home));
            }
            let no_xdg = PathEnv {
                xdg_config_home: None,
                ..env.clone()
            };
            let config = user_config_path(platform, &no_xdg).unwrap();
            assert!(text(&config).starts_with(home), "{config:?}");

            // No home: no default anywhere, and no value left behind for a process to read.
            let none = person_env(platform, None, Path::new(cfg));
            assert_eq!(default_meta_path(platform, &none), None);
            assert!(none.home.is_none());
            assert!(none.user_profile.is_none());
            assert!(none.home_drive.is_none());
            assert!(none.home_path.is_none());
            assert!(none.app_data.is_none());
            assert!(text(&user_config_path(platform, &none).unwrap()).starts_with(cfg));
        }

        // No home on this host: the alternatives are removed from every process.
        let bare = World::without_home();
        let names: Vec<String> = bare.env().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["XDG_CONFIG_HOME"]);
        for name in ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH", "APPDATA"] {
            assert!(bare.removed_env().contains(&name), "{name}");
        }
        let (platform, env) = bare.path_env();
        assert_eq!(
            resolve_person(platform, &env).unwrap().path,
            PersonPath::Unavailable
        );
        assert!(!bare.teardown_paths().contains(&bare.default_meta()));

        // Cleanup knows a meta project nobody made through `project_at`: the default one, and a
        // configured one.
        let w = World::new();
        assert!(w.teardown_paths().contains(&w.default_meta()));
        drop(Rdy::init(&w.default_meta(), "rdy-standard").expect("init the default meta"));
        assert!(w.teardown_paths().contains(&w.default_meta()));
        let moved = short_tempdir();
        let configured = moved.path().join("rules");
        w.person(&configured);
        assert!(w.teardown_paths().contains(&configured));
        let tracked = w.project();
        let paths = w.teardown_paths();
        assert!(paths.contains(&tracked.path().to_path_buf()));
        w.project_at(&w.default_meta().with_file_name("again"));
        w.project_at(&configured);
        let paths = w.teardown_paths();
        let unique: std::collections::BTreeSet<_> = paths.iter().collect();
        assert_eq!(
            unique.len(),
            paths.len(),
            "a path is stopped once: {paths:?}"
        );
    }

    /// The person variables a command would see, as the engine reads them: what the command
    /// sets, and nothing for a variable it removes. (A variable it leaves alone comes from the
    /// machine; the test below proves there is none.)
    fn env_as_read(cmd: &Command) -> PathEnv {
        let get = |name: &str| {
            let set = cmd
                .get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(name))
                .unwrap_or_else(|| panic!("{name} is inherited"));
            set.1.map(std::ffi::OsString::from)
        };
        PathEnv {
            xdg_config_home: get("XDG_CONFIG_HOME"),
            home: get("HOME"),
            user_profile: get("USERPROFILE"),
            home_drive: get("HOMEDRIVE"),
            home_path: get("HOMEPATH"),
            app_data: get("APPDATA"),
            ..PathEnv::default()
        }
    }

    /// Canary values for a person's home sit outside the fixture, in the command before the
    /// world isolates it. The instructions the engine finds are the fixture person project's,
    /// and no canary reaches a place the engine reads.
    #[test]
    fn wx25a_ain_reads_only_the_fixture_person_project() {
        let canary = short_tempdir();
        let canary_home = canary.path().join("home");
        let canary_cfg = canary.path().join("cfg");
        for world in [World::new(), World::without_home()] {
            let mut cmd = Command::new("x");
            for name in PERSON_VARS {
                cmd.env(name, &canary_home);
            }
            cmd.env("XDG_CONFIG_HOME", &canary_cfg)
                .env("USERNAME", "canary-user")
                .env("SSH_AUTH_SOCK", canary.path().join("agent"));
            world.isolate(&mut cmd);

            // no value of the command names the canary
            for (name, value) in cmd.get_envs() {
                let text = value.map(|v| v.to_string_lossy().into_owned());
                assert!(
                    text.is_none_or(
                        |t| !Path::new(&t).starts_with(canary.path()) && !t.contains("canary")
                    ),
                    "{name:?} carries a canary"
                );
            }

            // the engine, given those values, finds the fixture's person project or none
            let env = env_as_read(&cmd);
            let (platform, fixture) = world.path_env();
            assert_eq!(
                resolve_person(Platform::HOST, &env).unwrap().path,
                resolve_person(platform, &fixture).unwrap().path
            );
            match resolve_person(Platform::HOST, &env).unwrap().path {
                PersonPath::Default(p) => {
                    assert!(!world.no_home);
                    assert_eq!(p, world.default_meta());
                    assert!(p.starts_with(world.home.path()));
                }
                PersonPath::Unavailable => assert!(world.no_home),
                other => panic!("{other:?}"),
            }
        }

        // an instruction the person configures resolves to the fixture's own project
        let w = World::new();
        let mine = short_tempdir();
        w.person(mine.path());
        let mut cmd = Command::new("x");
        cmd.env("HOME", &canary_home);
        w.isolate(&mut cmd);
        let env = env_as_read(&cmd);
        assert!(
            user_config_path(Platform::HOST, &env)
                .unwrap()
                .starts_with(w.config_dir())
        );
        assert!(!canary.path().join("cfg").exists() && !canary_home.exists());
    }
}
