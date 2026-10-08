//! sinst1: the server instructions say to read in full before editing and to supersede only the
//! part a change retires; rdy_supersede's description names part.
use rdy_qa::fixtures::project;
use rdy_qa::mcp::McpClient;

#[test]
fn sinst1_instructions_name_reading_before_editing_and_partial_supersession() {
    let i = rdy_mcp::server::INSTRUCTIONS;
    assert!(
        i.contains("Before editing a block, read it with format markdown or yaml: compact lines are a summary."),
        "{i}"
    );
    assert!(
        i.contains("When a change retires only some of a decision's rules, supersede that part (rdy_supersede with part), not the whole decision."),
        "{i}"
    );
    assert!(
        i.contains("Brief before editing"),
        "guard: the earlier lines stay: {i}"
    );
    let (dir, r) = project();
    drop(r);
    let mut c = McpClient::start(dir.path());
    let tools = c.tools();
    let d = tools.iter().find(|t| t["name"] == "rdy_supersede").unwrap()["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        d.contains("pass part (the words of the rules it retires) and the rest stays in force"),
        "{d}"
    );
}
