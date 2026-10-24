use std::io;
use std::path::Path;
use std::process::Command;

/// What a `pub` subcommand printed, kept rather than discarded so a
/// future UI can show the user what actually happened (package
/// resolution output, version conflicts, etc.), not just pass/fail.
#[derive(Debug, Clone)]
pub struct PubOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

fn run_pub(project_dir: &Path, args: &[&str]) -> io::Result<PubOutput> {
    let mut command = Vec::with_capacity(args.len() + 1);
    command.push("pub");
    command.extend_from_slice(args);
    let output = Command::new("flutter").args(&command).current_dir(project_dir).output()?;
    Ok(PubOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// `flutter pub get` in `project_dir`.
pub fn pub_get(project_dir: &Path) -> io::Result<PubOutput> {
    run_pub(project_dir, &["get"])
}

/// `flutter pub add <package>` (or `dev:<package>` for a dev
/// dependency) in `project_dir`. `package` can carry a version
/// constraint itself (`"foo@^1.2.3"`), same as the CLI accepts.
pub fn add_package(project_dir: &Path, package: &str, dev: bool) -> io::Result<PubOutput> {
    let spec = if dev { format!("dev:{package}") } else { package.to_string() };
    run_pub(project_dir, &["add", &spec])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn flutter_available() -> bool {
        Command::new("flutter").arg("--version").output().is_ok()
    }

    /// The one checked-in-repo fixture these tests need: a real,
    /// already-`flutter create`d project. Rather than generating one at
    /// test time (slow — `flutter create` fetches packages) or
    /// asserting against a path outside this repo (fragile — it was a
    /// scratch directory from manual protocol verification, not meant
    /// to persist), skip gracefully when it isn't present. On a machine
    /// that has `flutter`, running `flutter create` once anywhere and
    /// pointing `FLUTTER_TEST_PROJECT` at it exercises this for real.
    fn test_project_dir() -> Option<std::path::PathBuf> {
        std::env::var_os("FLUTTER_TEST_PROJECT").map(std::path::PathBuf::from).filter(|p| p.is_dir())
    }

    #[test]
    fn adds_a_real_package_and_runs_pub_get_against_the_real_cli() {
        if !flutter_available() {
            eprintln!("skipping: `flutter` not found on PATH");
            return;
        }
        let Some(project_dir) = test_project_dir() else {
            eprintln!("skipping: set FLUTTER_TEST_PROJECT to a real `flutter create`d project to run this for real");
            return;
        };

        let pubspec_path = project_dir.join("pubspec.yaml");
        let before = fs::read_to_string(&pubspec_path).expect("fixture project should have a pubspec.yaml");
        assert!(
            !before.contains("path:"),
            "fixture project shouldn't already depend on `path` (test isn't exercising anything if so)"
        );

        let add = add_package(&project_dir, "path", false).expect("flutter pub add should run");
        assert!(add.success, "flutter pub add path failed:\n{}", add.stderr);

        let after = fs::read_to_string(&pubspec_path).unwrap();
        assert!(after.contains("path:"), "expected `path:` to show up in pubspec.yaml after adding it");

        let get = pub_get(&project_dir).expect("flutter pub get should run");
        assert!(get.success, "flutter pub get failed:\n{}", get.stderr);
    }
}
