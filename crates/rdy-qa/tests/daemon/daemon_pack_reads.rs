//! racy1: a daemon's cold start reads each pack-defining file once. The read counter is per
//! process and the area binary runs other tests beside this one, so the test re-runs itself as
//! a child that runs it alone (`--exact`, one thread) and measures there.
use rdy_qa::daemon::Hosted;
use serde_json::json;

const NAME: &str =
    "racy1_a_cold_start_reads_the_pack_files_once_and_the_watch_reads_none_while_they_are_old";

#[test]
fn racy1_a_cold_start_reads_the_pack_files_once_and_the_watch_reads_none_while_they_are_old() {
    if std::env::var_os("RDY_QA_RACY1_CHILD").is_none() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &rdy_qa::test_name!(NAME),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("RDY_QA_RACY1_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "stdout: {stdout}\nstderr: {stderr}");
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}\n{stderr}"
        );
        return;
    }
    let dir = rdy_qa::daemon::short_tempdir();
    drop(rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/proj.yaml"),
        "name: proj\nversion: '1'\nextends: rdy-standard\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy.yaml"), "pack: proj\n").unwrap();
    // past the racy window, so the watch may trust the stamps the open took (a ctime cannot be
    // set back, so wait it out)
    std::thread::sleep(rdy_daemon::watch::RACY + std::time::Duration::from_millis(500));
    let before = rdy_engine::pack_source_reads();
    let h = Hosted::start_in(dir);
    let rt = Hosted::rt();
    rt.block_on(async {
        let mut a = h.client(None, None).await;
        assert_eq!(a.ctl("status", json!({})).await.unwrap()["pack"], "proj");
        for i in 0..5 {
            a.ctl("new_doc", json!({"path": format!("d{i}"), "view": null}))
                .await
                .unwrap();
        }
    });
    assert_eq!(
        rdy_engine::pack_source_reads() - before,
        1,
        "the open's one read; the watch only stats"
    );
}
