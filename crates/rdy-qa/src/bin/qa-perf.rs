fn main() {
    let measures = rdy_qa::perf::run_workload();
    println!("{}", serde_json::to_string(&measures).expect("json"));
}
