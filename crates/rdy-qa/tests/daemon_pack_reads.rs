//! racy1: a daemon's cold start reads each pack-defining file once. Alone in its test binary:
//! the read counter is per process.
use rdy_qa::daemon::Hosted;
use serde_json::json;

#[test]
fn racy1_a_cold_start_reads_the_pack_files_once_and_the_watch_reads_none_while_they_are_old() {
    let dir = rdy_qa::daemon::short_tempdir();
    drop(rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap());
    std::fs::write(
        dir.path().join(".rdy/packs/proj.yaml"),
        "name: proj\nversion: '1'\nextends: rdy-standard\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".rdy/config.yaml"), "pack: proj\n").unwrap();
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
