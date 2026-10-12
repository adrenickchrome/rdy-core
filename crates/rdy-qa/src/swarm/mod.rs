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
//! - `script`: the closed lists of acts and the seeded generator.
//! - `journal`: what each actor did and saw, and the closed table of allowed refusals.
//! - `project`: the served `rdy-qa` project the actors act on, with one stable doc per area.
//! - `agent`: the scripted agents, each a thread with its own `rdy mcp` proxy process.
//! - `people`: the person, acting through a relaying agent and the closed list of commands.
pub mod agent;
pub mod journal;
pub mod people;
pub mod project;
pub mod script;
