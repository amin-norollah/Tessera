//! Phase 4 Milestone B, exercised over a real `Simulation`: `Recorder` writes
//! keyframes on schedule and streams every cycle's events to Arrow/Parquet
//! (P4-F1/F4), the resulting event count matches an independent from-seed
//! run collected in-memory, `SeekTo` via the written keyframes still matches
//! resimulation (G4.2 continues to hold once events also stream to disk),
//! and a run registers into the SQLite catalog with its provenance intact
//! (P4-F7, G4.5).

use std::fs::File;

use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tessera_core::{SimConfig, Simulation};
use tessera_model::Mesh2D;
use tessera_record::{
    run_collecting_events, Catalog, KeyframeReader, NewRun, Recorder, SeekEngine, SimSource,
};
use tessera_routing::{DimensionOrderXy, FirstAvailable, RoundRobin};
use tessera_traffic::Uniform;

fn mesh_cfg(seed: u64) -> SimConfig {
    SimConfig {
        buffer_depth: 8,
        num_vcs: 2,
        router_latency: 1,
        link_latency: 1,
        credit_delay: 1,
        packet_size: 4,
        seed,
    }
}

struct UniformXySource {
    cfg: SimConfig,
}

impl SimSource for UniformXySource {
    fn fresh(&self) -> Simulation {
        Simulation::new(
            self.cfg,
            Box::new(Mesh2D::new(4, 4)),
            Box::new(DimensionOrderXy),
            Box::new(FirstAvailable),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(RoundRobin::new()),
            Box::new(Uniform::new(16, 0.3, self.cfg.packet_size)),
        )
    }
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tessera_cli_recording_test_{name}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn recorder_streams_events_and_keyframes_matching_a_plain_run() {
    let source = UniformXySource { cfg: mesh_cfg(13) };
    let until = 300;
    let keyframe_interval = 50;

    let dir = scratch_dir("basic");
    std::fs::create_dir_all(&dir).unwrap();
    let keyframe_dir = dir.join("keyframes");
    let event_path = dir.join("events.parquet");

    // Record a run through `Recorder`.
    let mut sim = source.fresh();
    let mut recorder =
        Recorder::create(&keyframe_dir, keyframe_interval, &event_path, 256).unwrap();
    recorder.start(&mut sim).unwrap();
    while sim.cycle < until {
        sim.tick();
        recorder.observe(&mut sim).unwrap();
    }
    recorder.finish().unwrap();

    // An independent from-seed run, collected in-memory (Milestone A's
    // helper), is the oracle for how many events this scenario produces.
    let mut oracle = source.fresh();
    let oracle_events = run_collecting_events(&mut oracle, until);
    assert!(
        !oracle_events.is_empty(),
        "test scenario produced no events"
    );

    // The Parquet file holds exactly that many rows.
    let file = File::open(&event_path).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let mut total_rows = 0u64;
    for batch in reader {
        total_rows += batch.unwrap().num_rows() as u64;
    }
    assert_eq!(total_rows, oracle_events.len() as u64);

    // Keyframes landed on the configured cadence.
    let kf_reader = KeyframeReader::open(&keyframe_dir).unwrap();
    assert_eq!(kf_reader.nearest_le(0), Some(0));
    assert_eq!(kf_reader.nearest_le(49), Some(0));
    assert_eq!(kf_reader.nearest_le(50), Some(50));
    assert_eq!(kf_reader.nearest_le(299), Some(250));

    // SeekTo via the on-disk keyframes still matches a from-seed resimulation
    // (G4.2 holds once a real run is streaming events to disk too, not just
    // in the in-memory scenario `replay_equivalence.test.rs` covers).
    let seek_engine = SeekEngine::with_keyframes(&source, &kf_reader);
    for &n in &[10u64, 120, 275] {
        let seeked = seek_engine.seek_to(n).unwrap();
        let resimmed = seek_engine.resimulate(n);
        assert_eq!(seeked.cycle, resimmed.cycle);
        assert_eq!(
            seeked.metrics.injected_packets,
            resimmed.metrics.injected_packets
        );
        assert_eq!(
            seeked.metrics.ejected_packets,
            resimmed.metrics.ejected_packets
        );
    }

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_recorded_run_registers_in_the_catalog_with_provenance_intact() {
    let dir = scratch_dir("catalog");
    std::fs::create_dir_all(&dir).unwrap();
    let keyframe_dir = dir.join("keyframes");
    let event_path = dir.join("events.parquet");
    let catalog_path = dir.join("catalog.sqlite");

    let source = UniformXySource { cfg: mesh_cfg(21) };
    let mut sim = source.fresh();
    let mut recorder = Recorder::create(&keyframe_dir, 100, &event_path, 256).unwrap();
    recorder.start(&mut sim).unwrap();
    for _ in 0..200 {
        sim.tick();
        recorder.observe(&mut sim).unwrap();
    }
    recorder.finish().unwrap();

    let catalog = Catalog::open(&catalog_path).unwrap();
    let id = catalog
        .register_run(&NewRun {
            name: "uniform-xy-smoke".to_string(),
            config_text: "seed = 21\nwidth = 4\nheight = 4\n".to_string(),
            seed: 21,
            keyframe_dir: Some(keyframe_dir.display().to_string()),
            event_store: Some(event_path.display().to_string()),
            created_unix: 1_700_000_000,
        })
        .unwrap();

    let run = catalog.get_run(id).unwrap().expect("just-registered run");
    assert_eq!(run.name, "uniform-xy-smoke");
    assert_eq!(run.seed, 21);
    assert_eq!(
        run.event_store.as_deref(),
        Some(event_path.display().to_string().as_str())
    );
    // The catalog only indexes where the artifacts live; confirm they're
    // real, readable files, not just a stored path string.
    assert!(std::path::Path::new(&run.event_store.unwrap()).exists());
    assert!(std::path::Path::new(&run.keyframe_dir.unwrap()).exists());

    // Windows won't remove a file SQLite still has open; drop the connection
    // explicitly before cleanup rather than relying on end-of-scope order.
    drop(catalog);
    std::fs::remove_dir_all(&dir).unwrap();
}
