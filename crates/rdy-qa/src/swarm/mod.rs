//! The swarm simulation: eight scripted MCP agents and one person against one daemon.
//!
//! What it proves: with real processes and a real, unordered interleaving, the daemon loses
//! no acknowledged write, reports every collision, lands nothing past the gate, and gives
//! each reader exactly what the log holds. Every assertion is a property of the final state
//! or of the journal, never of an order of events.
//!
//! The agents are code, not models. Each agent runs a script that is a pure function of the
//! seed (`script`). No real-agent trial is planned (F1 "Scripted only", D-VF4RH).
//!
//! - `script`: the closed lists of acts, the seeded generator and the forced M3 scenarios
//!   (`m3_scenarios`: descriptors beside the script, run at their steps by the actors).
//! - `journal`: what each actor did and saw, the M3 route attempts (an attempt, a refusal, an
//!   interruption and an acknowledgement stay distinct), and the closed table of allowed refusals.
//! - `project`: the served `rdy-qa` project the actors act on, with one stable doc per area and four small M3 subjects.
//! - `agent`: the scripted agents, each a thread with its own `rdy mcp` proxy process.
//! - `people`: the person, acting through a relaying agent and the closed list of commands.
//! - `m3`: the bounded driver of the forced M3 scenarios, run by their writers on their own
//!   connections (chat first: `rdy_pending`, the card shown, the person's words, `rdy_answer`).
//! - `run`: one run: the actors, the kill and restart, the freeze and the drain.
//! - `oracle`: what the final state must hold, computed apart from the actors' paths.
pub mod agent;
pub mod journal;
pub mod m3;
pub mod oracle;
pub mod people;
pub mod project;
pub mod run;
pub mod script;
