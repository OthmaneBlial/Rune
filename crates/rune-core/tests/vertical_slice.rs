use rune_core::Session;
use rune_fs::SandboxedFileSystem;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_root() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rune-integration-test-{suffix}"))
}

#[test]
fn runs_a_real_pipeline_and_keeps_stderr_bounded() {
    let root = test_root();
    let mut session = Session::new(SandboxedFileSystem::new(&root).expect("root created"));

    let output = session
        .execute_line("mkdir -p project; cd project; echo hello | cat > note.txt; cat < note.txt");
    assert_eq!(output.status, 0);
    assert_eq!(output.stdout, "hello\n");
    assert!(output.stderr.is_empty());

    let missing = session.execute_line("cat missing.txt 2> error.txt");
    assert_eq!(missing.status, 1);
    assert!(missing.stdout.is_empty());
    assert!(missing.stderr.is_empty(), "stderr was redirected to a file");
    let error_file = session.execute_line("cat error.txt");
    assert_eq!(error_file.status, 0);
    assert!(error_file.stdout.contains("no such file or directory"));

    let escaped = session.execute_line("cd ../../outside");
    assert_eq!(escaped.status, 1);
    assert!(escaped.stderr.contains("escapes the Rune sandbox"));
    assert_eq!(session.current_directory(), "~/project");

    session.persist().expect("session persisted");
    let restored = Session::restore(SandboxedFileSystem::new(&root).expect("root reopened"));
    assert_eq!(restored.current_directory(), "~/project");
    assert!(restored
        .history()
        .iter()
        .any(|line| line == "cat error.txt"));

    std::fs::remove_dir_all(root).expect("test root removed");
}
