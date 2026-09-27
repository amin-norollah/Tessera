//! Reproducibility-bundle stub (P4-F9): exports a registered run's
//! provenance — config, seed, and where its artifacts live — as a small,
//! human-readable manifest. A later Phase 5 tool expands this into the full
//! paper-ready bundle (code snapshot, copied results, figures); this stub
//! references the run's keyframe/event-store paths rather than copying those
//! (potentially large) artifacts.

use std::fs;
use std::path::{Path, PathBuf};

use crate::catalog::RunRecord;
use crate::RecordError;

/// Writes `out_dir/config.toml` (the run's config text, verbatim) and
/// `out_dir/manifest.toml` (run identity, seed, and artifact paths — TOML
/// so it's both human-readable and machine-parseable) for `run`. Returns the
/// manifest's path.
pub fn export_bundle(run: &RunRecord, out_dir: impl AsRef<Path>) -> Result<PathBuf, RecordError> {
    let out_dir = out_dir.as_ref();
    fs::create_dir_all(out_dir)?;

    fs::write(out_dir.join("config.toml"), &run.config_text)?;

    let manifest_path = out_dir.join("manifest.toml");
    let manifest = format!(
        "run_id = {}\n\
         name = \"{}\"\n\
         seed = {}\n\
         created_unix = {}\n\
         config = \"config.toml\"\n\
         keyframe_dir = {}\n\
         event_store = {}\n",
        run.id,
        escape(&run.name),
        run.seed,
        run.created_unix,
        toml_opt_string(run.keyframe_dir.as_deref()),
        toml_opt_string(run.event_store.as_deref()),
    );
    fs::write(&manifest_path, manifest)?;
    Ok(manifest_path)
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn toml_opt_string(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{}\"", escape(s)),
        None => "\"\"".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tessera_record_bundle_test_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_run() -> RunRecord {
        RunRecord {
            id: 7,
            name: "uniform \"xy\" 4x4".to_string(),
            config_text: "seed = 21\nwidth = 4\nheight = 4\n".to_string(),
            seed: 21,
            keyframe_dir: Some("C:\\runs\\7\\keyframes".to_string()),
            event_store: Some("C:\\runs\\7\\events.parquet".to_string()),
            created_unix: 1_700_000_000,
        }
    }

    #[test]
    fn exports_a_well_formed_manifest_and_config_that_round_trip() {
        let dir = scratch_dir("basic");
        let run = sample_run();
        let manifest_path = export_bundle(&run, &dir).unwrap();

        assert_eq!(manifest_path, dir.join("manifest.toml"));
        let config_text = fs::read_to_string(dir.join("config.toml")).unwrap();
        assert_eq!(config_text, run.config_text);

        let manifest_text = fs::read_to_string(&manifest_path).unwrap();
        let parsed: toml::Value = toml::from_str(&manifest_text)
            .expect("manifest must be valid, parseable TOML, including an escaped name/paths");
        assert_eq!(parsed["run_id"].as_integer(), Some(7));
        assert_eq!(parsed["name"].as_str(), Some(run.name.as_str()));
        assert_eq!(parsed["seed"].as_integer(), Some(21));
        assert_eq!(parsed["created_unix"].as_integer(), Some(1_700_000_000));
        assert_eq!(parsed["config"].as_str(), Some("config.toml"));
        assert_eq!(parsed["keyframe_dir"].as_str(), run.keyframe_dir.as_deref());
        assert_eq!(parsed["event_store"].as_str(), run.event_store.as_deref());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_artifact_paths_export_as_empty_strings() {
        let dir = scratch_dir("no_artifacts");
        let mut run = sample_run();
        run.keyframe_dir = None;
        run.event_store = None;
        let manifest_path = export_bundle(&run, &dir).unwrap();

        let manifest_text = fs::read_to_string(&manifest_path).unwrap();
        let parsed: toml::Value = toml::from_str(&manifest_text).unwrap();
        assert_eq!(parsed["keyframe_dir"].as_str(), Some(""));
        assert_eq!(parsed["event_store"].as_str(), Some(""));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
