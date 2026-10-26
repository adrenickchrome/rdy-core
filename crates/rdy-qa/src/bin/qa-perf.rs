use rdy_qa::perf::{PerfMode, perf_mode};

fn main() {
    // first, while this process has one thread: the diagram crate's variables never reach it
    rdy_daemon::sys::scrub_mmdflux_environment_at_startup();
    // an argument that is not UTF-8 is not the flag, so it gets the usage text
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.into_string().unwrap_or_default())
        .collect();
    let mode = match perf_mode(&args) {
        Ok(mode) => mode,
        Err(usage) => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };
    let measures = match mode {
        PerfMode::Gate => rdy_qa::perf::run_workload(),
        PerfMode::Renderer10k => rdy_qa::perf::renderer_10k(),
    };
    println!("{}", serde_json::to_string(&measures).expect("json"));
}
