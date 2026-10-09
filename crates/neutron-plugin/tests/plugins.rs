//! The host against real child processes.
//!
//! Everything here spawns an actual program, writes a real request down a real
//! pipe, and reads what comes back. The unit tests prove the pieces parse and
//! validate; these prove the thing works, and — more to the point — that it
//! survives plugins which hang, crash, print nonsense, or never read their
//! input. Those are the cases a mock cannot check, because the failure being
//! guarded against *is* the pipe and the process.
//!
//! Unix only, because the plugins here are shell scripts. The host code they
//! exercise is platform-independent std; the one Windows-specific line in it
//! (`CREATE_NO_WINDOW`) is about whether a console flashes, which no test can
//! see anyway.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neutron_plugin::discover::PluginSet;
use neutron_plugin::host;

/// A plugins root under the system temp directory, emptied first so a previous
/// run cannot make this one pass.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("neutron-plugin-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Installs a one-command plugin whose program is `script`, and returns the
/// discovered set.
fn install(root: &Path, script: &str, needs: &str, timeout_ms: u64) -> PluginSet {
    let dir = root.join("test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        format!(
            r#"{{"protocol":1,"id":"test","name":"Test Plugin","run":"prog","commands":[
                {{"id":"go","title":"Go","needs":"{needs}","timeout_ms":{timeout_ms}}}]}}"#
        ),
    )
    .unwrap();

    let program = dir.join("prog");
    std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
    let mut perms = std::fs::metadata(&program).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&program, perms).unwrap();

    let set = PluginSet::load_from(root);
    assert!(set.errors.is_empty(), "{:?}", set.errors);
    set
}

/// Runs the installed plugin's only command.
fn run(set: &PluginSet, folder: &str, selection: &[&str]) -> host::Outcome {
    let (plugin, command) = set.find("test", "go").expect("plugin should have loaded");
    let paths: Vec<PathBuf> = selection.iter().map(PathBuf::from).collect();
    host::invoke(plugin, command, Path::new(folder), &paths)
}

#[test]
fn a_plugin_can_report_a_status_and_ask_for_a_refresh() {
    let root = scratch("happy");
    let set = install(
        &root,
        r#"echo '{"type":"status","text":"3 files hashed"}'
           echo '{"type":"refresh"}'"#,
        "selection",
        5_000,
    );

    let out = run(&set, "/tmp", &["/tmp/a.txt"]);
    assert_eq!(out.error, None);
    assert_eq!(out.status.as_deref(), Some("3 files hashed"));
    assert!(out.refresh);
}

#[test]
fn the_folder_and_selection_reach_the_plugin() {
    // The point of the whole protocol. The plugin reads its request from stdin
    // and echoes it back inside a status line, so a request that never
    // arrived, arrived empty, or arrived malformed all fail here.
    let root = scratch("request");
    let set = install(
        &root,
        r#"read line
           printf '{"type":"status","text":%s}\n' "$(printf '%s' "$line" | sed 's/\"/x/g; s/^/\"/; s/$/\"/')""#,
        "selection",
        5_000,
    );

    let out = run(&set, "/tmp/notes", &["/tmp/notes/a.txt", "/tmp/notes/b.txt"]);
    let status = out.status.unwrap_or_default();
    // Quotes were replaced to keep the echo valid JSON, so match on the shape
    // that survives that: xcommandx:xgox.
    assert!(status.contains("xcommandx:xgox"), "{status}");
    assert!(status.contains("/tmp/notes/b.txt"), "{status}");
    assert!(status.contains("xprotocolx:1"), "{status}");
}

#[test]
fn a_plugin_that_hangs_is_killed_on_its_deadline() {
    let root = scratch("hang");
    let set = install(&root, "sleep 30", "folder", 300);

    let started = Instant::now();
    let out = run(&set, "/tmp", &[]);
    let took = started.elapsed();

    // The deadline is what returns, not the sleep.
    assert!(took < Duration::from_secs(5), "took {took:?}");
    let error = out.error.expect("a hung plugin must report");
    assert!(error.contains("did not answer"), "{error}");
}

#[test]
fn a_plugin_that_hangs_without_reading_its_input_is_still_killed() {
    // The stdin writer has to be on its own thread for this to come back at
    // all: 1024 paths is well past a pipe buffer, so writing the request would
    // block forever against a plugin that never reads it.
    let root = scratch("nostdin");
    let set = install(&root, "sleep 30", "selection", 300);

    let paths: Vec<String> = (0..host::MAX_SELECTION)
        .map(|i| format!("/tmp/a-fairly-long-file-name-so-the-request-is-large-{i}.txt"))
        .collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let started = Instant::now();
    let out = run(&set, "/tmp", &refs);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(out.error.is_some_and(|e| e.contains("did not answer")));
}

#[test]
fn a_plugin_that_crashes_reports_its_own_stderr() {
    let root = scratch("crash");
    let set = install(
        &root,
        r#"echo "cannot find libfoo" >&2
           exit 3"#,
        "folder",
        5_000,
    );

    let out = run(&set, "/tmp", &[]);
    let error = out.error.expect("a failed plugin must report");
    // The plugin's own words, because "exit code 3" tells the user nothing.
    assert!(error.contains("cannot find libfoo"), "{error}");
}

#[test]
fn a_plugin_that_fails_silently_still_names_itself_and_its_exit_code() {
    let root = scratch("silent");
    let set = install(&root, "exit 9", "folder", 5_000);

    let error = run(&set, "/tmp", &[]).error.expect("must report");
    assert!(error.contains("Test Plugin"), "{error}");
    assert!(error.contains('9'), "{error}");
}

#[test]
fn a_plugins_own_error_message_wins_over_the_exit_code() {
    let root = scratch("ownerror");
    let set = install(
        &root,
        r#"echo '{"type":"error","message":"that file is not an image"}'
           exit 1"#,
        "selection",
        5_000,
    );

    assert_eq!(
        run(&set, "/tmp", &["/tmp/a.txt"]).error.as_deref(),
        Some("that file is not an image")
    );
}

#[test]
fn nonsense_on_stdout_is_reported_rather_than_ignored() {
    let root = scratch("garbage");
    let set = install(&root, "echo hello there", "folder", 5_000);

    let error = run(&set, "/tmp", &[]).error.expect("must report");
    assert!(error.contains("cannot read"), "{error}");
}

#[test]
fn a_plugin_printing_forever_is_cut_off_and_the_host_returns() {
    let root = scratch("runaway");
    let set = install(
        &root,
        r#"while true; do echo '{"type":"status","text":"working"}'; done"#,
        "folder",
        10_000,
    );

    let started = Instant::now();
    let out = run(&set, "/tmp", &[]);

    // Returns because the response cap fires, not because the deadline does.
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());

    // The whole outcome, not just the field being checked. This failed once in
    // CI with `left: None, right: Some("working")`, which says nothing about
    // *why* nothing was applied — the host reports every way a run can go wrong
    // through `error`, and that is the field that would have explained it.
    assert_eq!(
        out.status.as_deref(),
        Some("working"),
        "nothing was applied before the cap fired; whole outcome: {out:?}"
    );
}

#[test]
fn one_endless_line_does_not_grow_without_limit() {
    // A plugin printing megabytes with no newline would be buffered forever by
    // a naive line reader.
    let root = scratch("longline");
    let set = install(
        &root,
        r#"i=0
           while [ $i -lt 200 ]; do printf '%01000d' 0; i=$((i+1)); done
           echo '{"type":"status","text":"never seen"}'"#,
        "folder",
        10_000,
    );

    let out = run(&set, "/tmp", &[]);
    assert_eq!(out.status, None, "the line should have been refused");
    assert!(out.error.is_some());
}

#[test]
fn a_line_that_never_ends_is_refused_promptly_rather_than_at_the_deadline() {
    // No newline, ever. The cap has to fire while the line is still being
    // read; a reader that waited for the newline would buffer until the
    // deadline killed the plugin, holding everything it had read so far.
    let root = scratch("endless");
    let set = install(&root, r#"while true; do printf '%01000d' 0; done"#, "folder", 20_000);

    let started = Instant::now();
    let out = run(&set, "/tmp", &[]);

    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    let error = out.error.expect("an over-long line is a plugin error");
    assert!(error.contains("longer than"), "{error}");
}

#[test]
fn a_plugin_that_logs_heavily_to_stderr_is_not_cut_off() {
    // Far more than the kept tail and more than a pipe buffer. The stderr
    // reader used to stop after 8 KB and drop the pipe, so the next write
    // killed the plugin with SIGPIPE before it could answer.
    let root = scratch("chatty");
    let set = install(
        &root,
        r#"i=0
           while [ $i -lt 400 ]; do printf '%01000d\n' $i >&2; i=$((i+1)); done
           echo '{"type":"status","text":"done"}'"#,
        "folder",
        10_000,
    );

    let out = run(&set, "/tmp", &[]);
    assert_eq!(out.error, None, "{out:?}");
    assert_eq!(out.status.as_deref(), Some("done"), "{out:?}");
}

#[test]
fn a_failing_plugin_reports_the_end_of_its_stderr_not_the_beginning() {
    let root = scratch("tail");
    let set = install(
        &root,
        r#"i=0
           while [ $i -lt 100 ]; do printf 'noise %01000d\n' $i >&2; i=$((i+1)); done
           echo "the actual cause" >&2
           exit 2"#,
        "folder",
        10_000,
    );

    let error = run(&set, "/tmp", &[]).error.expect("must report");
    assert!(error.contains("the actual cause"), "{error}");
}

#[test]
fn a_control_character_in_a_status_line_cannot_reach_the_status_bar() {
    let root = scratch("sanitised");
    // A real carriage return, written by the plugin through printf, not the
    // two characters backslash-r.
    let set = install(
        &root,
        r#"printf '{"type":"status","text":"done\\rHACKED"}\n'"#,
        "folder",
        5_000,
    );

    let status = run(&set, "/tmp", &[]).status.expect("a status");
    assert!(!status.chars().any(char::is_control), "{status:?}");
    assert!(status.contains("done HACKED"), "{status:?}");
}

#[test]
fn only_a_bounded_number_of_paths_can_be_opened() {
    let root = scratch("opens");
    let set = install(
        &root,
        r#"i=0
           while [ $i -lt 40 ]; do printf '{"type":"open","path":"/tmp/f%d"}\n' $i; i=$((i+1)); done"#,
        "folder",
        5_000,
    );

    let out = run(&set, "/tmp", &[]);
    assert_eq!(out.open.len(), host::MAX_OPENS);
}

#[test]
fn an_unusable_path_is_dropped_rather_than_acted_on() {
    let root = scratch("badpath");
    let set = install(
        &root,
        r#"echo '{"type":"navigate","path":"   "}'
           echo '{"type":"status","text":"tried"}'"#,
        "folder",
        5_000,
    );

    let out = run(&set, "/tmp", &[]);
    assert_eq!(out.navigate, None);
    assert_eq!(out.status.as_deref(), Some("tried"));
}

#[test]
fn a_last_navigate_wins() {
    let root = scratch("navigate");
    let set = install(
        &root,
        r#"echo '{"type":"navigate","path":"/tmp/first"}'
           echo '{"type":"navigate","path":"/tmp/second"}'"#,
        "folder",
        5_000,
    );

    assert_eq!(run(&set, "/tmp", &[]).navigate, Some(PathBuf::from("/tmp/second")));
}

#[test]
fn a_plugin_that_says_nothing_and_succeeds_is_reported_as_silent() {
    // Not an error — but the caller has to be able to tell, or choosing the
    // command looks like it did nothing at all.
    let root = scratch("quiet");
    let set = install(&root, "exit 0", "folder", 5_000);

    let out = run(&set, "/tmp", &[]);
    assert_eq!(out.error, None);
    assert!(out.is_silent());
}

#[test]
fn a_selection_larger_than_the_limit_is_refused_without_running_anything() {
    let root = scratch("toomany");
    // A plugin that would fail loudly if it ever ran.
    let set = install(&root, "exit 42", "selection", 5_000);

    let paths: Vec<String> = (0..host::MAX_SELECTION + 1).map(|i| format!("/tmp/{i}")).collect();
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();

    let error = run(&set, "/tmp", &refs).error.expect("must refuse");
    assert!(error.contains(&host::MAX_SELECTION.to_string()), "{error}");
    assert!(!error.contains("42"), "the plugin should not have been started: {error}");
}

#[test]
fn a_program_that_is_not_executable_reports_a_reason() {
    let root = scratch("notexec");
    let dir = root.join("test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("plugin.json"),
        r#"{"protocol":1,"id":"test","name":"Test Plugin","run":"prog","commands":[
            {"id":"go","title":"Go","needs":"folder"}]}"#,
    )
    .unwrap();
    // Present, so discovery accepts it, but not runnable.
    std::fs::write(dir.join("prog"), b"not a program").unwrap();

    let set = PluginSet::load_from(&root);
    assert!(set.errors.is_empty(), "{:?}", set.errors);

    let error = run(&set, "/tmp", &[]).error.expect("must report");
    assert!(error.contains("prog"), "{error}");
}

#[test]
fn a_running_plugin_can_be_stopped() {
    // The deadline is generous here on purpose: what returns must be the Stop,
    // not the timeout.
    let root = scratch("stop");
    let set = install(&root, "sleep 30", "folder", 60_000);
    let (plugin, command) = set.find("test", "go").unwrap();

    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = std::sync::Arc::clone(&cancel);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    let started = Instant::now();
    let out = host::invoke_with_cancel(plugin, command, Path::new("/tmp"), &[], &cancel);

    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    let error = out.error.expect("stopping must be reported");
    assert!(error.contains("stopped"), "{error}");
}
