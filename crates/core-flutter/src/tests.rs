//! A running `flutter test` / `dart test`, understood. The test runner's JSON
//! reporter (`--reporter json`, one event per line) is folded into a
//! [`TestRun`] that always knows which tests passed, failed or are still going,
//! so a UI can show them as they finish.
//!
//! Event shapes (abridged) this understands:
//! `suite {suite:{id,path}}`, `group {group:{id,name}}`,
//! `testStart {test:{id,name,suiteID,groupIDs,line,column,url}}`,
//! `testDone {testID,result,skipped,hidden}`, `error {testID,error,stackTrace,isFailure}`,
//! `print {testID,message}`, `done {success}`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde_json::Value;

use crate::inspector::file_uri_to_path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestStatus {
    Running,
    Passed,
    /// An assertion failed.
    Failed,
    /// The test threw something that isn't an assertion failure.
    Errored,
    Skipped,
}

impl TestStatus {
    pub fn is_bad(self) -> bool {
        matches!(self, TestStatus::Failed | TestStatus::Errored)
    }
}

#[derive(Clone, Debug)]
pub struct TestCase {
    pub id: i64,
    /// The full name, including its group(s): `"Counter increments"`.
    pub name: String,
    pub group: String,
    pub path: PathBuf,
    /// 1-based, as the runner reports them; 0 if unknown.
    pub line: u32,
    pub status: TestStatus,
    pub error: Option<String>,
    pub stack: Option<String>,
    pub output: Vec<String>,
}

impl TestCase {
    /// One line saying why the test failed, for a note in the editor.
    /// Flutter's own error text is a generic "Test failed. See exception
    /// logs above."; the reason is in the printed output, so look there
    /// first: an `Expected:` / `Actual:` pair, or the line after "...was
    /// thrown...".
    pub fn failure_summary(&self) -> Option<String> {
        if !self.status.is_bad() {
            return None;
        }
        let text: Vec<&str> = self
            .output
            .iter()
            .chain(self.error.iter())
            .flat_map(|chunk| chunk.lines())
            .map(str::trim)
            .collect();
        if let Some(ix) = text.iter().position(|l| l.starts_with("Expected:")) {
            let actual = text[ix + 1..]
                .iter()
                .find(|l| l.starts_with("Actual:"))
                .map(|l| format!(" \u{b7} {l}"))
                .unwrap_or_default();
            return Some(format!("{}{actual}", text[ix]));
        }
        if let Some(ix) = text.iter().position(|l| l.contains("was thrown")) {
            if let Some(reason) = text[ix + 1..].iter().find(|l| !l.is_empty()) {
                return Some(reason.to_string());
            }
        }
        text.iter()
            .find(|l| !l.is_empty() && !l.starts_with("Test failed. See exception logs"))
            .map(|l| l.to_string())
            .or_else(|| Some("Test failed".to_string()))
    }

    /// The test's own name without its group prefix.
    pub fn short_name(&self) -> &str {
        self.name
            .strip_prefix(self.group.as_str())
            .map(str::trim_start)
            .filter(|rest| !rest.is_empty())
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub running: usize,
}

impl Counts {
    pub fn total(self) -> usize {
        self.passed + self.failed + self.skipped + self.running
    }
}

/// One group of tests within a file, for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupNode {
    pub name: String,
    pub tests: Vec<i64>,
}

/// One test file and its groups, for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuiteNode {
    pub path: PathBuf,
    pub groups: Vec<GroupNode>,
}

#[derive(Default)]
pub struct TestRun {
    pub tests: BTreeMap<i64, TestCase>,
    suites: HashMap<i64, PathBuf>,
    groups: HashMap<i64, String>,
    /// `Some(success)` once the runner reports `done`.
    pub done: Option<bool>,
}

impl TestRun {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one line of the runner's output into the run. Lines that are not
    /// JSON events (build chatter) are ignored. Returns whether anything changed.
    pub fn apply_line(&mut self, line: &str) -> bool {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            return false;
        };
        self.apply(&event)
    }

    pub fn apply(&mut self, event: &Value) -> bool {
        match event.get("type").and_then(Value::as_str) {
            Some("suite") => {
                let suite = &event["suite"];
                if let (Some(id), Some(path)) = (suite["id"].as_i64(), suite["path"].as_str()) {
                    self.suites.insert(id, PathBuf::from(path));
                }
                false
            }
            Some("group") => {
                let group = &event["group"];
                if let (Some(id), Some(name)) = (group["id"].as_i64(), group["name"].as_str()) {
                    self.groups.insert(id, name.to_string());
                }
                false
            }
            Some("testStart") => self.test_start(&event["test"]),
            Some("testDone") => self.test_done(event),
            Some("error") => self.test_error(event),
            Some("print") => {
                let (Some(id), Some(message)) = (event["testID"].as_i64(), event["message"].as_str()) else {
                    return false;
                };
                match self.tests.get_mut(&id) {
                    Some(test) => {
                        test.output.push(message.to_string());
                        true
                    }
                    None => false,
                }
            }
            Some("done") => {
                self.done = Some(event["success"].as_bool().unwrap_or(false));
                true
            }
            _ => false,
        }
    }

    fn test_start(&mut self, test: &Value) -> bool {
        let (Some(id), Some(name)) = (test["id"].as_i64(), test["name"].as_str()) else {
            return false;
        };
        // The runner "loads" each file as a pseudo-test; it is not one of yours.
        if name.starts_with("loading ") {
            return false;
        }
        let suite_path = test["suiteID"].as_i64().and_then(|s| self.suites.get(&s)).cloned();
        // Where the test is declared in *your* file. A test written through a
        // wrapper (`testWidgets` calls `test` inside `flutter_test`) reports
        // `url`/`line` inside the wrapper and puts your file in
        // `root_url`/`root_line`, so those win when present.
        let located = |url: &str, line: &str| {
            let path = test[url].as_str().and_then(file_uri_to_path)?;
            Some((path, test[line].as_u64().unwrap_or(0) as u32))
        };
        let (path, line) = located("root_url", "root_line")
            .or_else(|| located("url", "line"))
            // A `package:` url means the line is in someone else's code.
            .unwrap_or_else(|| (suite_path.unwrap_or_default(), 0));
        // The test's own group is the last (innermost) group id.
        let group = test["groupIDs"]
            .as_array()
            .and_then(|ids| ids.iter().rev().filter_map(Value::as_i64).find_map(|g| self.groups.get(&g)))
            .cloned()
            .unwrap_or_default();
        self.tests.insert(
            id,
            TestCase {
                id,
                name: name.to_string(),
                group,
                path,
                line,
                status: TestStatus::Running,
                error: None,
                stack: None,
                output: Vec::new(),
            },
        );
        true
    }

    fn test_done(&mut self, event: &Value) -> bool {
        let Some(id) = event["testID"].as_i64() else { return false };
        let hidden = event["hidden"].as_bool().unwrap_or(false);
        let result = event["result"].as_str().unwrap_or("success");
        let Some(test) = self.tests.get_mut(&id) else { return false };
        if event["skipped"].as_bool().unwrap_or(false) {
            test.status = TestStatus::Skipped;
            return true;
        }
        match result {
            "success" => {
                // Hidden passes are setUpAll / tearDownAll bookkeeping.
                if hidden {
                    self.tests.remove(&id);
                } else {
                    test.status = TestStatus::Passed;
                }
            }
            "failure" => test.status = TestStatus::Failed,
            _ => {
                // An `error` event may already have classified it as a failure.
                if test.status != TestStatus::Failed {
                    test.status = TestStatus::Errored;
                }
            }
        }
        true
    }

    fn test_error(&mut self, event: &Value) -> bool {
        let Some(id) = event["testID"].as_i64() else { return false };
        let Some(test) = self.tests.get_mut(&id) else { return false };
        let message = event["error"].as_str().unwrap_or("").trim().to_string();
        test.error = Some(match test.error.take() {
            Some(existing) => format!("{existing}\n\n{message}"),
            None => message,
        });
        if let Some(stack) = event["stackTrace"].as_str().filter(|s| !s.trim().is_empty()) {
            test.stack = Some(stack.trim().to_string());
        }
        test.status = if event["isFailure"].as_bool().unwrap_or(false) {
            TestStatus::Failed
        } else {
            TestStatus::Errored
        };
        true
    }

    pub fn counts(&self) -> Counts {
        let mut counts = Counts::default();
        for test in self.tests.values() {
            match test.status {
                TestStatus::Passed => counts.passed += 1,
                TestStatus::Failed | TestStatus::Errored => counts.failed += 1,
                TestStatus::Skipped => counts.skipped += 1,
                TestStatus::Running => counts.running += 1,
            }
        }
        counts
    }

    pub fn failed_tests(&self) -> Vec<&TestCase> {
        self.tests.values().filter(|t| t.status.is_bad()).collect()
    }

    /// The tests as files -> groups -> test ids, in the order they ran.
    pub fn outline(&self) -> Vec<SuiteNode> {
        let mut suites: Vec<SuiteNode> = Vec::new();
        for test in self.tests.values() {
            let suite = match suites.iter().position(|s| s.path == test.path) {
                Some(ix) => ix,
                None => {
                    suites.push(SuiteNode { path: test.path.clone(), groups: Vec::new() });
                    suites.len() - 1
                }
            };
            let groups = &mut suites[suite].groups;
            match groups.iter_mut().find(|g| g.name == test.group) {
                Some(group) => group.tests.push(test.id),
                None => groups.push(GroupNode { name: test.group.clone(), tests: vec![test.id] }),
            }
        }
        suites
    }
}

/// Whether a `pubspec.yaml` belongs to a Flutter package (so `flutter test`,
/// not `dart test`, runs its tests).
pub fn is_flutter_package(pubspec: &str) -> bool {
    pubspec.lines().any(|line| {
        let line = line.trim();
        line == "sdk: flutter" || line.starts_with("sdk: flutter ") || line == "flutter:" && pubspec.contains("flutter_test")
    })
}

/// A regular expression matching exactly these test names, for
/// `--name` (re-running just the failures).
pub fn name_filter_regex(names: &[&str]) -> String {
    let escaped: Vec<String> = names
        .iter()
        .map(|name| {
            let mut out = String::with_capacity(name.len());
            for c in name.chars() {
                if "\\.^$|?*+()[]{}".contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        })
        .collect();
    format!("^({})$", escaped.join("|"))
}

/// Arguments for the test runner's JSON reporter: `flutter test` or `dart test`,
/// optionally limited to one file and/or to tests by exact-substring name
/// (`plain_name`) or regular expression (`name_regex`).
pub fn test_args(
    flutter: bool,
    rel_file: Option<&str>,
    plain_name: Option<&str>,
    name_regex: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = if flutter {
        vec!["test".into(), "--reporter".into(), "json".into()]
    } else {
        vec!["test".into(), "-r".into(), "json".into()]
    };
    if let Some(name) = plain_name {
        args.push("--plain-name".into());
        args.push(name.into());
    }
    if let Some(regex) = name_regex {
        args.push("--name".into());
        args.push(regex.into());
    }
    if let Some(file) = rel_file {
        args.push(file.into());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real-shaped stream: one file, a group with a pass and a failure, a
    /// skipped test, a hidden setUpAll, and a loading pseudo-test.
    const STREAM: &str = r#"Running Gradle task...
{"protocolVersion":"0.1.1","runnerVersion":"1.25.8","type":"start","time":0}
{"suite":{"id":0,"platform":"vm","path":"/p/test/counter_test.dart"},"type":"suite","time":0}
{"test":{"id":1,"name":"loading /p/test/counter_test.dart","suiteID":0,"groupIDs":[],"metadata":{"skip":false}},"type":"testStart","time":1}
{"testID":1,"result":"success","skipped":false,"hidden":true,"type":"testDone","time":900}
{"group":{"id":2,"suiteID":0,"parentID":null,"name":"","metadata":{"skip":false},"testCount":4},"type":"group","time":901}
{"group":{"id":3,"suiteID":0,"parentID":2,"name":"Counter","metadata":{"skip":false},"testCount":4,"line":4,"column":3,"url":"file:///p/test/counter_test.dart"},"type":"group","time":902}
{"test":{"id":4,"name":"Counter (setUpAll)","suiteID":0,"groupIDs":[2,3],"metadata":{"skip":false}},"type":"testStart","time":903}
{"testID":4,"result":"success","skipped":false,"hidden":true,"type":"testDone","time":904}
{"test":{"id":5,"name":"Counter starts at zero","suiteID":0,"groupIDs":[2,3],"metadata":{"skip":false},"line":6,"column":5,"url":"file:///p/test/counter_test.dart"},"type":"testStart","time":905}
{"testID":5,"result":"success","skipped":false,"hidden":false,"type":"testDone","time":910}
{"test":{"id":6,"name":"Counter increments","suiteID":0,"groupIDs":[2,3],"metadata":{"skip":false},"line":10,"column":5,"url":"file:///p/test/counter_test.dart"},"type":"testStart","time":911}
{"testID":6,"messageType":"print","message":"counter is 1","type":"print","time":912}
{"testID":6,"error":"Expected: <2>\n  Actual: <1>\n","stackTrace":"package:test_api  expect\ntest/counter_test.dart 12:7  main.<fn>.<fn>\n","isFailure":true,"type":"error","time":913}
{"testID":6,"result":"failure","skipped":false,"hidden":false,"type":"testDone","time":914}
{"test":{"id":7,"name":"Counter wraps","suiteID":0,"groupIDs":[2,3],"metadata":{"skip":true},"line":16,"column":5,"url":"file:///p/test/counter_test.dart"},"type":"testStart","time":915}
{"testID":7,"result":"success","skipped":true,"hidden":false,"type":"testDone","time":916}
{"success":false,"type":"done","time":920}
"#;

    fn run() -> TestRun {
        let mut run = TestRun::new();
        for line in STREAM.lines() {
            run.apply_line(line);
        }
        run
    }

    #[test]
    fn folds_a_real_stream_into_results() {
        let run = run();
        assert_eq!(run.counts(), Counts { passed: 1, failed: 1, skipped: 1, running: 0 });
        assert_eq!(run.counts().total(), 3, "the loading pseudo-test and setUpAll are not counted");
        assert_eq!(run.done, Some(false));
    }

    /// Captured from a real `dart test -r json` run (runner 1.33): the shape this
    /// parser must keep understanding.
    #[test]
    fn understands_a_real_captured_dart_test_run() {
        let mut run = TestRun::new();
        for line in include_str!("fixtures/dart_test_json_real.jsonl").lines() {
            run.apply_line(line);
        }
        let by_name = |name: &str| run.tests.values().find(|t| t.name == name).unwrap();
        assert_eq!(run.counts(), Counts { passed: 2, failed: 1, skipped: 1, running: 0 });
        assert_eq!(by_name("Counter starts at zero").status, TestStatus::Passed);
        assert_eq!(by_name("top level").status, TestStatus::Passed);
        assert_eq!(by_name("Counter increments").status, TestStatus::Failed);
        assert_eq!(by_name("Counter skipped one").status, TestStatus::Skipped);
        assert_eq!(by_name("top level").group, "", "a test outside any group");
        let failed = by_name("Counter increments");
        assert_eq!(failed.line, 7);
        assert_eq!(failed.path.to_str().unwrap(), "/pkg/test/counter_test.dart");
        assert_eq!(failed.output, vec!["counter is 1"]);
        assert_eq!(run.done, Some(false));
        assert!(run.tests.values().all(|t| !t.name.contains("loading") && !t.name.contains("All)")));
    }

    /// `testWidgets` reports the wrapper's location in `url`/`line` and the
    /// test's own in `root_url`/`root_line`.
    #[test]
    fn widget_tests_are_located_in_your_file_not_flutter_test() {
        let mut run = TestRun::new();
        run.apply_line(r#"{"suite":{"id":0,"platform":"vm","path":"/app/test/home_test.dart"},"type":"suite","time":0}"#);
        run.apply_line(r#"{"test":{"id":3,"name":"shows the title","suiteID":0,"groupIDs":[],"metadata":{"skip":false},"line":172,"column":5,"url":"package:flutter_test/src/widget_tester.dart","root_line":12,"root_column":3,"root_url":"file:///app/test/home_test.dart"},"type":"testStart","time":5}"#);
        run.apply_line(r#"{"test":{"id":4,"name":"no location","suiteID":0,"groupIDs":[],"metadata":{"skip":false},"line":9,"url":"package:other/x.dart"},"type":"testStart","time":6}"#);
        let test = &run.tests[&3];
        assert_eq!(test.line, 12);
        assert_eq!(test.path.to_str(), Some("/app/test/home_test.dart"));
        let test = &run.tests[&4];
        assert_eq!((test.path.to_str(), test.line), (Some("/app/test/home_test.dart"), 0));
    }

    #[test]
    fn a_failure_is_summed_up_from_the_printed_output() {
        let mut test = TestCase {
            id: 1,
            name: "t".into(),
            group: String::new(),
            path: PathBuf::new(),
            line: 1,
            status: TestStatus::Failed,
            error: Some("Test failed. See exception logs above.\nThe test description was: t".into()),
            stack: None,
            output: vec!["══╡ EXCEPTION CAUGHT BY FLUTTER TEST FRAMEWORK ╞══\nThe following TestFailure was thrown running a test:\nExpected: <2>\n  Actual: <1>\n".into()],
        };
        assert_eq!(test.failure_summary().as_deref(), Some("Expected: <2> \u{b7} Actual: <1>"));
        test.output = vec!["The following assertion was thrown building Foo:\nA RenderFlex overflowed by 12 pixels.".into()];
        assert_eq!(test.failure_summary().as_deref(), Some("A RenderFlex overflowed by 12 pixels."));
        test.status = TestStatus::Passed;
        assert_eq!(test.failure_summary(), None);
    }

    #[test]
    fn failures_carry_their_error_stack_and_output() {
        let run = run();
        let failed = run.failed_tests();
        assert_eq!(failed.len(), 1);
        let t = failed[0];
        assert_eq!(t.status, TestStatus::Failed);
        assert!(t.error.as_deref().unwrap().contains("Expected: <2>"));
        assert!(t.stack.as_deref().unwrap().contains("counter_test.dart 12:7"));
        assert_eq!(t.output, vec!["counter is 1"]);
        assert_eq!((t.path.to_str().unwrap(), t.line), ("/p/test/counter_test.dart", 10));
    }

    #[test]
    fn names_split_into_group_and_short_name() {
        let run = run();
        let t = &run.tests[&6];
        assert_eq!(t.group, "Counter");
        assert_eq!(t.short_name(), "increments");
        // A top-level test has no group to strip.
        let mut lone = TestRun::new();
        lone.apply_line(r#"{"test":{"id":1,"name":"adds","suiteID":0,"groupIDs":[],"line":3,"url":"file:///p/a_test.dart"},"type":"testStart"}"#);
        assert_eq!(lone.tests[&1].short_name(), "adds");
    }

    #[test]
    fn the_outline_groups_tests_by_file_and_group_in_run_order() {
        let run = run();
        assert_eq!(
            run.outline(),
            vec![SuiteNode {
                path: PathBuf::from("/p/test/counter_test.dart"),
                groups: vec![GroupNode { name: "Counter".into(), tests: vec![5, 6, 7] }],
            }]
        );
    }

    #[test]
    fn a_test_still_running_is_counted_as_running() {
        let mut run = TestRun::new();
        run.apply_line(r#"{"test":{"id":9,"name":"slow","suiteID":0,"groupIDs":[],"line":1,"url":"file:///p/a_test.dart"},"type":"testStart"}"#);
        assert_eq!(run.counts().running, 1);
        assert_eq!(run.done, None);
    }

    #[test]
    fn a_thrown_non_assertion_error_is_errored_not_failed() {
        let mut run = TestRun::new();
        run.apply_line(r#"{"test":{"id":2,"name":"boom","suiteID":0,"groupIDs":[],"line":1,"url":"file:///p/a_test.dart"},"type":"testStart"}"#);
        run.apply_line(r#"{"testID":2,"error":"Bad state: no element","stackTrace":"","isFailure":false,"type":"error"}"#);
        run.apply_line(r#"{"testID":2,"result":"error","skipped":false,"hidden":false,"type":"testDone"}"#);
        assert_eq!(run.tests[&2].status, TestStatus::Errored);
        assert_eq!(run.counts().failed, 1, "errors count as failures in the summary");
    }

    #[test]
    fn non_json_lines_and_unknown_events_change_nothing() {
        let mut run = TestRun::new();
        assert!(!run.apply_line("Resolving dependencies..."));
        assert!(!run.apply_line(r#"{"type":"debug","message":"x"}"#));
        assert!(run.tests.is_empty());
    }

    #[test]
    fn builds_the_runner_command_line() {
        assert_eq!(test_args(true, None, None, None), ["test", "--reporter", "json"]);
        assert_eq!(test_args(false, Some("test/a_test.dart"), None, None), ["test", "-r", "json", "test/a_test.dart"]);
        assert_eq!(
            test_args(true, Some("test/a_test.dart"), Some("Counter increments"), None),
            ["test", "--reporter", "json", "--plain-name", "Counter increments", "test/a_test.dart"]
        );
        assert_eq!(
            test_args(true, None, None, Some("^(a|b)$")),
            ["test", "--reporter", "json", "--name", "^(a|b)$"]
        );
    }

    #[test]
    fn detects_flutter_packages_and_builds_name_filters() {
        assert!(is_flutter_package("dependencies:\n  flutter:\n    sdk: flutter\n"));
        assert!(!is_flutter_package("name: cli_tool\ndependencies:\n  args: ^2.0.0\n"));
        assert_eq!(name_filter_regex(&["Counter increments"]), "^(Counter increments)$");
        assert_eq!(
            name_filter_regex(&["a.b (x)", "c|d"]),
            r"^(a\.b \(x\)|c\|d)$",
            "regex metacharacters in names are escaped"
        );
    }
}
