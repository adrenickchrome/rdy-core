//! Not a check: the gate's env probe. With `RDY_QA_ENV_PROBE` naming a file, it writes the
//! working directory and environment this binary got, so the gate can launch area binaries
//! directly with the same ones. Without it, it does nothing.

#[test]
fn env_probe_dump() {
    let Some(file) = std::env::var_os(rdy_qa::gate_steps::ENV_PROBE_VAR) else {
        return;
    };
    std::fs::write(file, rdy_qa::gate_steps::env_probe_record()).expect("write the env probe");
}
