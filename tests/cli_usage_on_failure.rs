//! Regression test for issue #23: a run that fails partway through — after at least one real LLM
//! call already succeeded, so real cost was already incurred — must still report the accumulated
//! usage/cost summary on stderr instead of silently discarding it.
//!
//! Reproduced for real during this session's execution-verification round: a discourse critic
//! call exhausted its retry budget on a live `haiku` response, and `main()` printed only the
//! error line, with the cost of the 4 already-completed lens-review calls nowhere in the output —
//! no `report.md`, no `state.json`, and (unlike a clean success) no usage/cost line either.
//!
//! This drives the actual compiled `research` binary as a subprocess rather than testing
//! in-process, since the behavior under test is `main()`'s own process-exit orchestration, not
//! anything reachable by calling a function directly.

use std::process::Command;

/// Writes a fake `claude` CLI replacement that always returns a `selected` lens id absent from
/// the spec, regardless of the prompt. `lens::select_lenses` is the very first LLM call
/// `review` makes (when `--lenses` isn't given), so this is the smallest possible reproduction of
/// "a call succeeded (and was recorded/costed) but the run still ultimately failed" — the
/// call itself succeeds, and only the *validation of its result* fails afterward.
fn write_fake_claude_selecting_invalid_lens(dir: &std::path::Path) -> std::path::PathBuf {
    let inner = serde_json::json!({"selected": ["not_a_real_lens"]});
    let outer = serde_json::json!({"result": serde_json::to_string(&inner).unwrap()});
    let json_path = dir.join("fake_claude_result.json");
    std::fs::write(&json_path, serde_json::to_string(&outer).unwrap()).unwrap();

    let script_path = dir.join("fake_claude.sh");
    std::fs::write(
        &script_path,
        format!(
            "#!/bin/sh\ncat >/dev/null\ncat \"{}\"\n",
            json_path.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script_path
}

#[test]
fn usage_summary_is_printed_on_a_mid_run_failure() {
    let dir = std::env::temp_dir().join(format!(
        "research_loop_cli_usage_test_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let spec_path = dir.join("spec.toml");
    std::fs::write(
        &spec_path,
        "name = \"test\"\nlabels = [\"x\"]\n\n[[lenses]]\nid = \"market_dynamics\"\ntitle = \"Market Dynamics\"\n",
    )
    .unwrap();

    let doc_path = dir.join("document.md");
    std::fs::write(&doc_path, "## Section\nSome content.\n").unwrap();

    let bin = write_fake_claude_selecting_invalid_lens(&dir);
    let out_dir = dir.join("out");

    let out = Command::new(env!("CARGO_BIN_EXE_research"))
        .arg("--claude-bin")
        .arg(&bin)
        .arg("--retries")
        .arg("0")
        .arg("review")
        .arg("--spec")
        .arg(&spec_path)
        .arg("--document")
        .arg(&doc_path)
        .arg("--out")
        .arg(&out_dir)
        .output()
        .expect("failed to run the research binary");

    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !out.status.success(),
        "selecting a lens id absent from the spec must make the run fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("LLM calls"),
        "the usage summary must be printed even on failure; stderr was: {stderr}"
    );
    assert!(
        stderr.contains("Error:"),
        "the error itself must still be printed; stderr was: {stderr}"
    );
}
