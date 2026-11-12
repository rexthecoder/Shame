//! The AI panel: a rail beside the editor for machine-learning work in a
//! Python project. It shows what the project runs on (interpreter,
//! accelerator, key packages, Hugging Face models on disk) and puts the
//! usual actions one click away: install requirements, run a training
//! script, open TensorBoard, open or create notebooks. Runs go to the Runs
//! tab like every other run.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::*;

use crate::run_sessions::RunSpec;
use crate::{DartIde, tokens};

/// Packages worth showing by name when installed, in display order.
const KEY_PACKAGES: &[&str] = &[
    "torch", "torchvision", "jax", "tensorflow", "transformers", "datasets", "accelerate", "peft", "trl",
    "diffusers", "sentence-transformers", "vllm", "mlx", "mlx-lm", "llama-cpp-python", "openai", "anthropic",
    "langchain", "numpy", "pandas", "scikit-learn", "lightning", "wandb", "tensorboard", "jupyter",
];

/// The probe run with the project's interpreter: Python version, installed
/// distributions and what torch sees. One process, JSON out.
const PROBE: &str = r#"
import json, platform, sys
out = {"python": platform.python_version(), "machine": platform.machine(), "packages": {}}
try:
    import importlib.metadata as md
    for d in md.distributions():
        name = (d.metadata.get("Name") or "").lower()
        if name:
            out["packages"][name] = d.version
except Exception as e:
    out["packages_error"] = str(e)
if "torch" in out["packages"]:
    try:
        import torch
        out["cuda"] = bool(torch.cuda.is_available())
        if out["cuda"]:
            out["cuda_devices"] = [torch.cuda.get_device_name(i) for i in range(torch.cuda.device_count())]
            out["cuda_version"] = torch.version.cuda
        mps = getattr(torch.backends, "mps", None)
        out["mps"] = bool(mps and mps.is_available())
    except Exception as e:
        out["torch_error"] = str(e).splitlines()[0][:200]
print(json.dumps(out))
"#;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProbeResult {
    pub(crate) python: String,
    pub(crate) machine: String,
    pub(crate) packages: Vec<(String, String)>,
    pub(crate) cuda: Option<bool>,
    pub(crate) cuda_devices: Vec<String>,
    pub(crate) mps: Option<bool>,
    pub(crate) torch_error: Option<String>,
}

pub(crate) fn parse_probe(json: &str) -> Result<ProbeResult, String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| e.to_string())?;
    let mut packages: Vec<(String, String)> = v
        .get("packages")
        .and_then(|p| p.as_object())
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    packages.sort();
    Ok(ProbeResult {
        python: v.get("python").and_then(|s| s.as_str()).unwrap_or("").into(),
        machine: v.get("machine").and_then(|s| s.as_str()).unwrap_or("").into(),
        packages,
        cuda: v.get("cuda").and_then(|b| b.as_bool()),
        cuda_devices: v
            .get("cuda_devices")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        mps: v.get("mps").and_then(|b| b.as_bool()),
        torch_error: v.get("torch_error").and_then(|s| s.as_str()).map(str::to_string),
    })
}

/// A model or dataset in the Hugging Face hub cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HfEntry {
    /// `org/name`.
    pub(crate) repo: String,
    pub(crate) is_dataset: bool,
    pub(crate) bytes: u64,
}

/// The hub cache: `$HF_HUB_CACHE`, `$HF_HOME/hub`, else
/// `~/.cache/huggingface/hub`.
pub(crate) fn hf_cache_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("HF_HOME") {
        return Some(PathBuf::from(p).join("hub"));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache/huggingface/hub"))
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            // Snapshots are symlinks into blobs: count blobs only.
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

/// `models--org--name` and `datasets--org--name` folders, largest first.
pub(crate) fn scan_hf_cache(dir: &Path) -> Vec<HfEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<HfEntry> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let (is_dataset, rest) = if let Some(r) = name.strip_prefix("models--") {
                (false, r.to_string())
            } else if let Some(r) = name.strip_prefix("datasets--") {
                (true, r.to_string())
            } else {
                return None;
            };
            Some(HfEntry { repo: rest.replacen("--", "/", 1), is_dataset, bytes: dir_size(&e.path().join("blobs")) })
        })
        .collect();
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.repo.cmp(&b.repo)));
    out
}

pub(crate) fn fmt_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024. && u + 1 < UNITS.len() {
        v /= 1024.;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// How to install this project's dependencies, as (title, program, args).
pub(crate) fn install_plan(root: &Path, python: &Path) -> Option<(String, PathBuf, Vec<String>)> {
    let py = python.to_path_buf();
    if root.join("uv.lock").exists() {
        if let Some(uv) = editor_view::python::find_tool("uv", None) {
            return Some(("uv sync".into(), uv, vec!["sync".into()]));
        }
    }
    if root.join("requirements.txt").exists() {
        let args = ["-m", "pip", "install", "-r", "requirements.txt"].map(String::from).to_vec();
        return Some(("pip install -r requirements.txt".into(), py, args));
    }
    if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
        let args = ["-m", "pip", "install", "-e", "."].map(String::from).to_vec();
        return Some(("pip install -e .".into(), py, args));
    }
    None
}

/// Python files that look like training entry points: `train*.py`,
/// `finetune*.py`, `fine_tune*.py`, at the root or in `scripts/`, `src/`.
pub(crate) fn training_scripts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in [root.to_path_buf(), root.join("scripts"), root.join("src")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let looks = name.ends_with(".py")
                && (name.starts_with("train") || name.starts_with("finetune") || name.starts_with("fine_tune")
                    || name.contains("_train"));
            if looks && e.path().is_file() {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

/// Where training runs log, for TensorBoard: the first that exists.
pub(crate) fn tensorboard_logdir(root: &Path) -> Option<PathBuf> {
    ["runs", "logs", "lightning_logs", "tb_logs", "outputs"].iter().map(|d| root.join(d)).find(|p| p.is_dir())
}

/// Notebooks in the project (not in hidden or environment folders).
pub(crate) fn find_notebooks(root: &Path, limit: usize) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: usize, limit: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 || out.len() >= limit {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "venv" | "env" | "__pycache__" | "site-packages") {
                continue;
            }
            let path = e.path();
            if path.is_dir() {
                walk(&path, depth + 1, limit, out);
            } else if name.ends_with(".ipynb") && out.len() < limit {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, 0, limit, &mut out);
    out
}

#[derive(Clone)]
pub(crate) enum Probe {
    Loading,
    Ready(ProbeResult),
    Failed(String),
}

#[derive(Default)]
pub(crate) struct AiPanelState {
    pub(crate) open: bool,
    pub(crate) env: Option<editor_view::python::PythonEnv>,
    pub(crate) probe: Option<Probe>,
    pub(crate) chip: Option<String>,
    pub(crate) models: Option<Vec<HfEntry>>,
    pub(crate) show_all_packages: bool,
    /// Project scans, refreshed with the rest (not on every frame).
    pub(crate) scripts: Vec<PathBuf>,
    pub(crate) notebooks: Vec<PathBuf>,
    pub(crate) logdir: Option<PathBuf>,
}

impl DartIde {
    pub(crate) fn toggle_ai_panel(&mut self, cx: &mut Context<Self>) {
        self.ai_panel.open = !self.ai_panel.open;
        if self.ai_panel.open && self.ai_panel.probe.is_none() {
            self.refresh_ai_panel(cx);
        }
        cx.notify();
    }

    /// Re-read everything: interpreter, packages, accelerator, models.
    pub(crate) fn refresh_ai_panel(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.ai_panel.probe = Some(Probe::Loading);
        self.ai_panel.models = None;
        self.ai_panel.scripts = training_scripts(&root);
        self.ai_panel.notebooks = find_notebooks(&root, 12);
        self.ai_panel.logdir = tensorboard_logdir(&root);
        cx.spawn(async move |weak, cx| {
            let (env, probe, chip, models) = cx
                .background_executor()
                .spawn(async move {
                    let env = editor_view::python::detect_env(Some(&root));
                    let probe = match &env {
                        None => Probe::Failed("No Python interpreter found. Create a .venv or install Python 3.".into()),
                        Some(env) => match std::process::Command::new(&env.python).args(["-c", PROBE]).current_dir(&root).output() {
                            Ok(out) if out.status.success() => match parse_probe(&String::from_utf8_lossy(&out.stdout)) {
                                Ok(p) => Probe::Ready(p),
                                Err(e) => Probe::Failed(format!("Couldn't read the interpreter's answer: {e}")),
                            },
                            Ok(out) => Probe::Failed(String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").to_string()),
                            Err(e) => Probe::Failed(e.to_string()),
                        },
                    };
                    let chip = std::process::Command::new("sysctl")
                        .args(["-n", "machdep.cpu.brand_string"])
                        .output()
                        .ok()
                        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                        .filter(|s| !s.is_empty());
                    let models = hf_cache_dir().map(|d| scan_hf_cache(&d)).unwrap_or_default();
                    (env, probe, chip, models)
                })
                .await;
            let _ = weak.update(cx, |this, cx| {
                this.ai_panel.env = env;
                this.ai_panel.probe = Some(probe);
                this.ai_panel.chip = chip;
                this.ai_panel.models = Some(models);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn ai_install_requirements(&mut self, cx: &mut Context<Self>) {
        let Some(env) = editor_view::python::detect_env(Some(&self.root)) else {
            self.show_toast("No Python interpreter found.", true, cx);
            return;
        };
        let Some((title, program, args)) = install_plan(&self.root, &env.python) else {
            self.show_toast("No requirements.txt, pyproject.toml or uv.lock in this project.", true, cx);
            return;
        };
        self.start_run(RunSpec { title, program, args, cwd: self.root.clone(), env: Vec::new() }, cx);
    }

    fn ai_run_script(&mut self, script: PathBuf, cx: &mut Context<Self>) {
        let Some(env) = editor_view::python::detect_env(Some(&self.root)) else {
            self.show_toast("No Python interpreter found.", true, cx);
            return;
        };
        self.save_dirty_python_buffers(cx);
        let rel = script.strip_prefix(&self.root).unwrap_or(&script).to_string_lossy().into_owned();
        let title = script.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| rel.clone());
        self.start_run(RunSpec { title, program: env.python, args: vec![rel], cwd: self.root.clone(), env: Vec::new() }, cx);
    }

    /// Start TensorBoard as a run and show it in the web preview rail.
    fn ai_open_tensorboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = editor_view::python::detect_env(Some(&self.root));
        let Some(tb) = editor_view::python::find_tool("tensorboard", env.as_ref()) else {
            self.show_toast("TensorBoard isn't installed. Run `pip install tensorboard`.", true, cx);
            return;
        };
        let logdir = tensorboard_logdir(&self.root).unwrap_or_else(|| self.root.join("runs"));
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(6006);
        let rel = logdir.strip_prefix(&self.root).unwrap_or(&logdir).to_string_lossy().into_owned();
        self.start_run(
            RunSpec {
                title: format!("tensorboard {rel}"),
                program: tb,
                args: vec!["--logdir".into(), rel, "--host".into(), "127.0.0.1".into(), "--port".into(), port.to_string()],
                cwd: self.root.clone(),
                env: Vec::new(),
            },
            cx,
        );
        // Give it a moment to bind before the preview loads.
        let url = format!("http://127.0.0.1:{port}/");
        cx.spawn_in(window, async move |weak, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(2500)).await;
            let _ = weak.update_in(cx, |this, window, cx| {
                this.web_preview_url.update(cx, |s, cx| s.set_value(url, window, cx));
                this.web_preview_open = true;
                this.web_preview_go(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn ai_new_notebook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut path = self.root.join("Untitled.ipynb");
        let mut n = 1;
        while path.exists() {
            path = self.root.join(format!("Untitled{n}.ipynb"));
            n += 1;
        }
        if let Err(e) = std::fs::write(&path, core_jupyter::Notebook::empty().to_json()) {
            self.show_toast(format!("Couldn't create the notebook: {e}"), true, cx);
            return;
        }
        self.ai_panel.notebooks = find_notebooks(&self.root, 12);
        self.open_path(path, window, cx);
    }

    pub(crate) fn render_ai_rail(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (fg, muted, border, green, primary, red) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.green, theme.primary, theme.red);
        let hover = theme.list_hover;
        let mono = theme.mono_font_family.clone();

        let section = |title: &'static str| {
            div()
                .px(px(tokens::SPACE_5))
                .pt(px(tokens::SPACE_6))
                .pb(px(tokens::SPACE_3))
                .text_size(px(tokens::UI_XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(muted)
                .child(title.to_uppercase())
        };
        let kv = |k: &'static str, v: String, tone: Option<Hsla>| {
            h_flex()
                .px(px(tokens::SPACE_5))
                .h(px(tokens::TREE_ROW_H - tokens::SPACE_2))
                .gap(px(tokens::SPACE_4))
                .items_center()
                .text_size(px(tokens::UI_SM))
                .child(div().w(px(92.)).flex_none().text_color(muted).child(k))
                .child(div().flex_1().min_w(px(0.)).truncate().text_color(tone.unwrap_or(fg)).child(v))
        };
        let tile = |id: String, icon: IconName, title: String, sub: String| {
            h_flex()
                .id(SharedString::from(format!("ai-tile-{id}")))
                .mx(px(tokens::SPACE_3))
                .px(px(tokens::SPACE_3))
                .h(px(tokens::TREE_ROW_H + tokens::SPACE_5))
                .gap(px(tokens::SPACE_4))
                .items_center()
                .rounded(px(tokens::RADIUS_SM))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(
                    div()
                        .size(px(tokens::TREE_ROW_H))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(tokens::RADIUS_SM))
                        .bg(tokens::accent_dim())
                        .child(Icon::new(icon).small().text_color(primary)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w(px(0.))
                        .child(div().text_size(px(tokens::UI_SM)).text_color(fg).truncate().child(title))
                        .child(div().text_size(px(tokens::UI_XS)).text_color(muted).truncate().child(sub)),
                )
        };

        let mut body = v_flex().w_full().pb(px(tokens::SPACE_6));

        // Environment
        body = body.child(section("Environment"));
        match (&self.ai_panel.env, &self.ai_panel.probe) {
            (_, None) | (_, Some(Probe::Loading)) => {
                body = body.child(kv("Interpreter", "Looking…".into(), Some(muted)));
            }
            (env, Some(Probe::Failed(e))) => {
                if let Some(env) = env {
                    body = body.child(kv("Interpreter", env.label(), None));
                }
                body = body.child(
                    div().px(px(tokens::SPACE_5)).text_size(px(tokens::UI_XS)).text_color(red).child(e.clone()),
                );
            }
            (env, Some(Probe::Ready(p))) => {
                let label = env.as_ref().map(|e| e.label()).unwrap_or_default();
                body = body.child(kv("Interpreter", format!("{label} · Python {}", p.python), None));
                if let Some(env) = env {
                    body = body.child(
                        div()
                            .px(px(tokens::SPACE_5))
                            .pb(px(tokens::SPACE_2))
                            .font_family(mono.clone())
                            .text_size(px(tokens::UI_XS))
                            .text_color(muted)
                            .truncate()
                            .child(env.python.to_string_lossy().into_owned()),
                    );
                }
                // Accelerator
                let accel: (String, Option<Hsla>) = if p.cuda == Some(true) {
                    let name = p.cuda_devices.first().cloned().unwrap_or_else(|| "GPU".into());
                    (format!("CUDA · {name}"), Some(green))
                } else if p.mps == Some(true) {
                    ("Apple GPU (MPS)".into(), Some(green))
                } else if let Some(e) = &p.torch_error {
                    (format!("torch failed to load: {e}"), Some(red))
                } else if p.packages.iter().any(|(n, _)| n == "torch") {
                    ("CPU only".into(), None)
                } else {
                    ("Install torch to use the GPU".into(), Some(muted))
                };
                body = body.child(kv("Accelerator", accel.0, accel.1));
                if let Some(chip) = &self.ai_panel.chip {
                    body = body.child(kv("Machine", format!("{chip} · {}", p.machine), Some(muted)));
                }

                // Packages
                body = body.child(section("Packages"));
                let key: Vec<&(String, String)> = KEY_PACKAGES
                    .iter()
                    .filter_map(|k| p.packages.iter().find(|(n, _)| n == k))
                    .collect();
                let list: Vec<&(String, String)> =
                    if self.ai_panel.show_all_packages { p.packages.iter().collect() } else { key };
                if list.is_empty() {
                    body = body.child(kv("ML packages", "none installed".into(), Some(muted)));
                }
                for (name, version) in list {
                    body = body.child(
                        h_flex()
                            .px(px(tokens::SPACE_5))
                            .h(px(tokens::TREE_ROW_H - tokens::SPACE_3))
                            .items_center()
                            .text_size(px(tokens::UI_SM))
                            .child(div().flex_1().min_w(px(0.)).truncate().text_color(fg).child(name.clone()))
                            .child(div().font_family(mono.clone()).text_size(px(tokens::UI_XS)).text_color(muted).child(version.clone())),
                    );
                }
                let all = self.ai_panel.show_all_packages;
                body = body.child(
                    div()
                        .id("ai-all-packages")
                        .px(px(tokens::SPACE_5))
                        .pt(px(tokens::SPACE_2))
                        .text_size(px(tokens::UI_XS))
                        .text_color(primary)
                        .cursor_pointer()
                        .child(if all { "Show key packages".to_string() } else { format!("Show all {} packages", p.packages.len()) })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.ai_panel.show_all_packages = !all;
                            cx.notify();
                        })),
                );
            }
        }

        // Actions
        body = body.child(section("Actions"));
        let env_python = self.ai_panel.env.as_ref().map(|e| e.python.clone()).unwrap_or_default();
        if let Some((title, _, _)) = install_plan(&self.root, &env_python) {
            body = body.child(
                tile("install".into(), IconName::SquareTerminal, "Install requirements".into(), title)
                    .on_click(cx.listener(|this, _, _, cx| this.ai_install_requirements(cx))),
            );
        }
        for script in self.ai_panel.scripts.clone() {
            let rel = script.strip_prefix(&self.root).unwrap_or(&script).to_string_lossy().into_owned();
            body = body.child(
                tile(format!("train-{rel}"), IconName::Play, format!("Run {rel}"), "Training script · output in Runs".into())
                    .on_click(cx.listener(move |this, _, _, cx| this.ai_run_script(script.clone(), cx))),
            );
        }
        let logdir = self
            .ai_panel
            .logdir
            .clone()
            .map(|d| d.strip_prefix(&self.root).unwrap_or(&d).to_string_lossy().into_owned())
            .unwrap_or_else(|| "runs (not created yet)".into());
        body = body.child(
            tile("tensorboard".into(), IconName::ChartPie, "TensorBoard".into(), format!("Logs in {logdir} · opens in Web Preview"))
                .on_click(cx.listener(|this, _, window, cx| this.ai_open_tensorboard(window, cx))),
        );
        body = body.child(
            tile("new-notebook".into(), IconName::BookOpen, "New notebook".into(), "Untitled.ipynb in the project root".into())
                .on_click(cx.listener(|this, _, window, cx| this.ai_new_notebook(window, cx))),
        );

        // Notebooks
        let notebooks = self.ai_panel.notebooks.clone();
        if !notebooks.is_empty() {
            body = body.child(section("Notebooks"));
            for nb in notebooks {
                let rel = nb.strip_prefix(&self.root).unwrap_or(&nb).to_string_lossy().into_owned();
                body = body.child(
                    h_flex()
                        .id(SharedString::from(format!("ai-nb-{rel}")))
                        .mx(px(tokens::SPACE_3))
                        .px(px(tokens::SPACE_3))
                        .h(px(tokens::TREE_ROW_H))
                        .gap(px(tokens::SPACE_3))
                        .items_center()
                        .rounded(px(tokens::RADIUS_SM))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(Icon::new(IconName::BookOpen).xsmall().text_color(muted))
                        .child(div().flex_1().min_w(px(0.)).truncate().text_size(px(tokens::UI_SM)).text_color(fg).child(rel))
                        .on_click(cx.listener(move |this, _, window, cx| this.open_path(nb.clone(), window, cx))),
                );
            }
        }

        // Hugging Face models
        body = body.child(section("Hugging Face cache"));
        match &self.ai_panel.models {
            None => body = body.child(kv("Models", "Looking…".into(), Some(muted))),
            Some(models) if models.is_empty() => {
                body = body.child(kv("Models", "none downloaded".into(), Some(muted)));
            }
            Some(models) => {
                let total: u64 = models.iter().map(|m| m.bytes).sum();
                body = body.child(kv("On disk", format!("{} · {} repos", fmt_bytes(total), models.len()), Some(muted)));
                for m in models.iter().take(20) {
                    body = body.child(
                        h_flex()
                            .px(px(tokens::SPACE_5))
                            .h(px(tokens::TREE_ROW_H - tokens::SPACE_2))
                            .gap(px(tokens::SPACE_3))
                            .items_center()
                            .text_size(px(tokens::UI_SM))
                            .child(
                                Icon::new(if m.is_dataset { IconName::FileText } else { IconName::HardDrive })
                                    .xsmall()
                                    .text_color(muted),
                            )
                            .child(div().flex_1().min_w(px(0.)).truncate().text_color(fg).child(m.repo.clone()))
                            .child(div().text_size(px(tokens::UI_XS)).text_color(muted).child(fmt_bytes(m.bytes))),
                    );
                }
            }
        }

        let header = h_flex()
            .h(px(tokens::PANEL_BAR_H))
            .flex_none()
            .px(px(tokens::SPACE_5))
            .gap(px(tokens::SPACE_3))
            .items_center()
            .border_b_1()
            .border_color(border)
            .child(Icon::new(IconName::Cpu).small().text_color(primary))
            .child(div().flex_1().text_size(px(tokens::UI_SM)).font_weight(FontWeight::MEDIUM).text_color(fg).child("AI"))
            .child(
                div()
                    .id("ai-refresh")
                    .p(px(tokens::SPACE_2))
                    .rounded(px(tokens::RADIUS_BADGE))
                    .cursor_pointer()
                    .text_color(muted)
                    .hover(move |s| s.bg(hover).text_color(fg))
                    .child(Icon::new(IconName::RefreshCw).xsmall())
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_ai_panel(cx))),
            );

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .border_1()
            .border_color(border)
            .rounded(px(tokens::RADIUS_CARD))
            .overflow_hidden()
            .child(header)
            .child(div().id("ai-rail-scroll").flex_1().min_h(px(0.)).overflow_y_scroll().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{fmt_bytes, install_plan, parse_probe, scan_hf_cache, tensorboard_logdir, training_scripts};
    use std::path::Path;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("shame-ai-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_the_probe() {
        let p = parse_probe(r#"{"python":"3.12.1","machine":"arm64","packages":{"torch":"2.5.0","numpy":"2.1"},"cuda":false,"mps":true}"#).unwrap();
        assert_eq!(p.python, "3.12.1");
        assert_eq!(p.mps, Some(true));
        assert_eq!(p.packages, vec![("numpy".into(), "2.1".into()), ("torch".into(), "2.5.0".into())]);
        assert!(parse_probe("not json").is_err());
    }

    #[test]
    fn scans_the_hub_cache() {
        let d = tempdir("hf");
        std::fs::create_dir_all(d.join("models--meta-llama--Llama-3.2-1B/blobs")).unwrap();
        std::fs::write(d.join("models--meta-llama--Llama-3.2-1B/blobs/abc"), vec![0u8; 2048]).unwrap();
        std::fs::create_dir_all(d.join("datasets--org--data--v2/blobs")).unwrap();
        std::fs::create_dir_all(d.join(".locks")).unwrap();
        let entries = scan_hf_cache(&d);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].repo, "meta-llama/Llama-3.2-1B");
        assert_eq!(entries[0].bytes, 2048);
        assert_eq!(entries[1].repo, "org/data--v2");
        assert!(entries[1].is_dataset);
        assert_eq!(fmt_bytes(2048), "2.0 KB");
        assert_eq!(fmt_bytes(12), "12 B");
    }

    #[test]
    fn finds_scripts_logs_and_install_commands() {
        let d = tempdir("proj");
        std::fs::write(d.join("train.py"), "").unwrap();
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(d.join("scripts/finetune_lora.py"), "").unwrap();
        std::fs::write(d.join("model.py"), "").unwrap();
        let scripts: Vec<String> = training_scripts(&d)
            .iter()
            .map(|p| p.strip_prefix(&d).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(scripts, vec!["scripts/finetune_lora.py", "train.py"]);

        assert_eq!(tensorboard_logdir(&d), None);
        std::fs::create_dir_all(d.join("lightning_logs")).unwrap();
        assert_eq!(tensorboard_logdir(&d), Some(d.join("lightning_logs")));

        assert!(install_plan(&d, Path::new("/py")).is_none());
        std::fs::write(d.join("requirements.txt"), "torch\n").unwrap();
        let (title, prog, args) = install_plan(&d, Path::new("/py")).unwrap();
        assert_eq!(title, "pip install -r requirements.txt");
        assert_eq!(prog, Path::new("/py"));
        assert_eq!(args, vec!["-m", "pip", "install", "-r", "requirements.txt"]);
    }
}
