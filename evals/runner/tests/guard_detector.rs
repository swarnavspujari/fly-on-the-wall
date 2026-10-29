//! The evals-lock guard's impostor detector (.github/workflows/evals-guard.yml),
//! run exactly as the workflow runs it: the Python block is extracted from
//! the workflow file and fed JSON workflow fixtures. Skips when no Python 3
//! is on PATH.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

fn detector_source() -> String {
    let wf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/evals-guard.yml");
    let text = std::fs::read_to_string(&wf).expect("read evals-guard.yml");
    let start = text
        .find("python3 -I - <<'PY'\n")
        .map(|i| i + "python3 -I - <<'PY'\n".len())
        .expect("detector heredoc in evals-guard.yml");
    let end = start
        + text[start..]
            .find("\n          PY\n")
            .expect("end of the detector heredoc");
    // The workflow indents the script by 10 spaces.
    text[start..end]
        .lines()
        .map(|l| l.strip_prefix("          ").unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|p| {
        Command::new(p)
            .args([
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 8) else 1)",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Detector output for `workflows` (file name → workflow JSON).
fn detect(py: &str, workflows: &[(&str, &str)]) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    for (name, json) in workflows {
        std::fs::write(dir.path().join(format!("{name}.json")), json).unwrap();
    }
    let mut child = Command::new(py)
        .args(["-I", "-"])
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(detector_source().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "detector failed");
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn the_guard_detector_flags_impostors_and_only_impostors() {
    let Some(py) = python() else {
        eprintln!("SKIP: no Python 3 on PATH");
        return;
    };
    // The real repo's expression names (matrix only) stay clean.
    let clean = detect(
        py,
        &[(
            "ci.yml",
            r#"{"jobs": {
                "rust": {"name": "Rust (${{ matrix.os }})", "strategy": {"matrix": {"os": ["windows-latest", "ubuntu-22.04"]}}},
                "bundle": {"name": "Bundle (${{ matrix.name }})", "strategy": {"matrix": {"include": [{"name": "macos"}]}}},
                "ci-ok": {"name": "ci-ok"},
                "review": {"name": "Security review"}
            }}"#,
        )],
    );
    assert!(clean.is_empty(), "{clean:?}");

    let cases = [
        // literal names and ids
        ("literal-id", r#"{"jobs": {"evals-lock": {}}}"#),
        (
            "literal-name",
            r#"{"jobs": {"x": {"name": "evals-security"}}}"#,
        ),
        // a PR-controlled branch name
        (
            "head-ref",
            r#"{"jobs": {"x": {"name": "${{ github.head_ref }}"}}}"#,
        ),
        (
            "inputs",
            r#"{"jobs": {"x": {"name": "check ${{ inputs.which }}"}}}"#,
        ),
        // literal pieces spelling a check name
        (
            "format",
            r#"{"jobs": {"x": {"name": "${{ format('{0}als-security','ev') }}"}}}"#,
        ),
        (
            "pieces",
            r#"{"jobs": {"x": {"name": "ev${{ matrix.a }}-lock", "strategy": {"matrix": {"a": ["als"]}}}}}"#,
        ),
        // a matrix value that is a check name
        (
            "matrix",
            r#"{"jobs": {"x": {"name": "${{ matrix.n }}", "strategy": {"matrix": {"n": ["evals-quality"]}}}}}"#,
        ),
        ("unparseable", r#""unparseable""#),
    ];
    for (name, json) in cases {
        let out = detect(py, &[(name, json)]);
        assert_eq!(out.len(), 1, "{name}: {out:?}");
        assert!(out[0].starts_with(name), "{name}: {out:?}");
    }
}
