//! Python project commands: run the current file and pytest with the
//! project's own interpreter, each as a session in the Runs tab.

use editor_view::python::{self as py, PythonEnv};

use super::*;

/// Quote `s` for a POSIX shell, leaving plain words as they are.
pub(crate) fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@+".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

impl DartIde {
    /// A Python project is open: a Python marker at the root, or an open
    /// `.py` buffer.
    pub(crate) fn is_python_workspace(&self) -> bool {
        const MARKERS: &[&str] =
            &["pyproject.toml", "requirements.txt", "setup.py", "uv.lock", "environment.yml", ".venv"];
        (self.has_workspace && MARKERS.iter().any(|m| self.root.join(m).exists()))
            || self.tabs.iter().any(|t| {
                t.path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()) == Some("py")
            })
    }

    /// The project root for Python commands: the nearest Python project
    /// around `file`, else the workspace root.
    pub(crate) fn python_root(&self, file: Option<&Path>) -> PathBuf {
        file.and_then(py::project_root_for)
            .filter(|r| r.starts_with(&self.root) || !self.has_workspace)
            .unwrap_or_else(|| self.root.clone())
    }

    pub(crate) fn python_env(&self, root: &Path) -> Option<PythonEnv> {
        py::detect_env(Some(root))
    }

    fn active_python_file(&self) -> Option<PathBuf> {
        let tab = self.tabs.get(self.active_tab)?;
        tab.path
            .as_ref()
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("py"))
            .cloned()
    }

    /// Run `args` with the project's interpreter, from `root`, as a run
    /// session in the Runs tab.
    fn run_python(&mut self, title: String, root: &Path, args: Vec<String>, cx: &mut Context<Self>) {
        let Some(env) = self.python_env(root) else {
            self.show_toast("No Python interpreter found. Create a .venv or install Python 3.", true, cx);
            return;
        };
        let spec = crate::run_sessions::RunSpec {
            title,
            program: env.python,
            args,
            cwd: root.to_path_buf(),
            env: Vec::new(),
            display: None,
        };
        self.start_run(spec, cx);
    }

    /// "Python: Run Current File".
    pub(crate) fn run_python_file(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.active_python_file() else {
            self.show_toast("Open a .py file to run it.", true, cx);
            return;
        };
        self.save_dirty_python_buffers(cx);
        let root = self.python_root(Some(&file));
        let rel = file.strip_prefix(&root).unwrap_or(&file).to_string_lossy().into_owned();
        let title = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| rel.clone());
        self.run_python(title, &root, vec![rel], cx);
    }

    /// pytest on the whole project, a file, or one test (`file::name`).
    pub(crate) fn run_pytest(&mut self, file: Option<PathBuf>, test: Option<String>, cx: &mut Context<Self>) {
        self.save_dirty_python_buffers(cx);
        let root = self.python_root(file.as_deref());
        let mut args = vec!["-m".to_string(), "pytest".to_string()];
        let mut title = "pytest".to_string();
        if let Some(file) = &file {
            let mut target = file.strip_prefix(&root).unwrap_or(file).to_string_lossy().into_owned();
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            title = format!("pytest {name}");
            if let Some(test) = test {
                target.push_str("::");
                target.push_str(&test);
                title = format!("pytest {test}");
            }
            args.push(target);
        }
        args.push("-q".to_string());
        self.run_python(title, &root, args, cx);
    }

    pub(crate) fn run_pytest_in_active_file(&mut self, cx: &mut Context<Self>) {
        match self.active_python_file() {
            Some(file) => self.run_pytest(Some(file), None, cx),
            None => self.show_toast("Open a Python test file first.", true, cx),
        }
    }

    /// Save dirty Python buffers so the run sees what's on screen.
    pub(crate) fn save_dirty_python_buffers(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            let is_py = tab.path.as_ref().and_then(|p| p.extension()).and_then(|e| e.to_str()) == Some("py");
            if is_py {
                tab.editor.update(cx, |editor, cx| {
                    // Unformatted: a formatting save finishes later,
                    // after the run has already read the file.
                    if editor.buffer().is_dirty() {
                        editor.save_without_format(cx);
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn quotes_only_what_needs_it() {
        assert_eq!(sh_quote("train.py"), "train.py");
        assert_eq!(sh_quote("tests/test_a.py::test_x"), "tests/test_a.py::test_x");
        assert_eq!(sh_quote("/My Projects/a.py"), "'/My Projects/a.py'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }
}
