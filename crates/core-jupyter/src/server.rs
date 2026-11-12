//! Jupyter Server: start a local one, or point at a remote one, and call
//! its REST API (kernel specs, kernels).

use std::io::{BufRead as _, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Where a Jupyter Server is and how to authenticate to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// `http(s)://host[:port][/base]`, no trailing slash.
    pub base_url: String,
    pub token: Option<String>,
}

impl ServerInfo {
    /// Read a server URL as Jupyter prints it or a browser shows it:
    /// `http://127.0.0.1:8888/lab?token=abc`, `https://host/user/me/?token=x`,
    /// or just `https://host`. The `lab`, `tree` and `notebooks` page parts
    /// are dropped; a JupyterHub `/user/<name>` prefix is kept.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        if is_colab_url(url) {
            return Err(COLAB_UNSUPPORTED.into());
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("a Jupyter server URL starts with http:// or https://".into());
        }
        let (without_query, query) = match url.split_once('?') {
            Some((u, q)) => (u, Some(q)),
            None => (url, None),
        };
        let token = query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")).map(str::to_string));
        let mut base = without_query.trim_end_matches('/').to_string();
        for page in ["/lab", "/tree", "/notebooks", "/api"] {
            if let Some(i) = base.find(page) {
                let rest = &base[i + page.len()..];
                if rest.is_empty() || rest.starts_with('/') {
                    base.truncate(i);
                    break;
                }
            }
        }
        let host = base.split_once("://").map_or("", |(_, rest)| rest);
        if host.is_empty() || host.starts_with('/') {
            return Err("the URL has no host".into());
        }
        Ok(Self { base_url: base, token: token.filter(|t| !t.is_empty()) })
    }

    pub fn is_local(&self) -> bool {
        let host = self.base_url.split("://").nth(1).unwrap_or("");
        host.starts_with("127.0.0.1") || host.starts_with("localhost") || host.starts_with("[::1]")
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub(crate) fn auth(&self) -> Option<String> {
        self.token.as_ref().map(|t| format!("token {t}"))
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn check(resp: ureq::http::Response<ureq::Body>) -> Result<Value, String> {
        let status = resp.status();
        let text = resp.into_body().read_to_string().unwrap_or_default();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err("the server refused the token (401/403); check the URL's ?token=".into());
        }
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.get("message").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| text.chars().take(200).collect());
            return Err(format!("server error {status}: {msg}"));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| format!("unexpected reply: {e}"))
    }

    fn unreachable(&self, e: ureq::Error) -> String {
        format!("can't reach {}: {e}", self.base_url)
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        let mut req = Self::agent().get(&self.url(path));
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        Self::check(req.call().map_err(|e| self.unreachable(e))?)
    }

    pub fn post(&self, path: &str, body: Value) -> Result<Value, String> {
        let mut req = Self::agent().post(&self.url(path));
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        let req = req.header("Content-Type", "application/json");
        Self::check(req.send(body.to_string()).map_err(|e| self.unreachable(e))?)
    }

    pub fn delete(&self, path: &str) -> Result<(), String> {
        let mut req = Self::agent().delete(&self.url(path));
        if let Some(auth) = self.auth() {
            req = req.header("Authorization", &auth);
        }
        Self::check(req.call().map_err(|e| self.unreachable(e))?).map(|_| ())
    }

    /// The server is up and the token works.
    pub fn status(&self) -> Result<(), String> {
        self.get("/api/status").map(|_| ())
    }

    /// Installed kernels, the default first.
    pub fn kernel_specs(&self) -> Result<Vec<KernelSpec>, String> {
        let v = self.get("/api/kernelspecs")?;
        let default = v.get("default").and_then(Value::as_str).unwrap_or("").to_string();
        let mut specs: Vec<KernelSpec> = v
            .get("kernelspecs")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(name, s)| KernelSpec {
                        name: name.clone(),
                        display_name: s.pointer("/spec/display_name").and_then(Value::as_str).unwrap_or(name).to_string(),
                        language: s.pointer("/spec/language").and_then(Value::as_str).unwrap_or("").to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        specs.sort_by_key(|s| (s.name != default, s.display_name.clone()));
        Ok(specs)
    }

    /// Start a kernel (in `path`, relative to the server root); returns its id.
    pub fn start_kernel(&self, name: &str, path: Option<&str>) -> Result<String, String> {
        let mut body = json!({ "name": name });
        if let Some(path) = path {
            body["path"] = json!(path);
        }
        let v = self.post("/api/kernels", body)?;
        v.get("id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "the server returned no kernel id".into())
    }

    pub fn interrupt_kernel(&self, id: &str) -> Result<(), String> {
        self.post(&format!("/api/kernels/{id}/interrupt"), json!({})).map(|_| ())
    }

    pub fn restart_kernel(&self, id: &str) -> Result<(), String> {
        self.post(&format!("/api/kernels/{id}/restart"), json!({})).map(|_| ())
    }

    pub fn shutdown_kernel(&self, id: &str) -> Result<(), String> {
        self.delete(&format!("/api/kernels/{id}"))
    }

    /// `ws(s)://…/api/kernels/{id}/channels?session_id=…`.
    pub fn channels_url(&self, kernel_id: &str, session_id: &str) -> String {
        let ws = match self.base_url.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => format!("ws://{}", self.base_url.trim_start_matches("http://")),
        };
        let mut url = format!("{ws}/api/kernels/{kernel_id}/channels?session_id={session_id}");
        // Some proxies drop the Authorization header on upgrade.
        if let Some(t) = &self.token {
            url.push_str("&token=");
            url.push_str(t);
        }
        url
    }
}

/// Why a Colab link can't be used as a server URL (see docs/colab.md).
pub const COLAB_UNSUPPORTED: &str = "Google Colab runtimes aren't Jupyter servers you can reach by URL: \
Colab only hands them to Google's own signed-in clients (its website and its VS Code extension). \
Use a Jupyter server URL instead: a local `jupyter server`, JupyterHub, Kaggle, Vertex AI Workbench or RunPod.";

/// A link to Colab itself (a notebook or the Colab site), not to a Jupyter server.
pub fn is_colab_url(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', ':']).next().unwrap_or("");
    host == "colab.research.google.com" || host == "colab.google" || host.ends_with(".colab.googleusercontent.com")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSpec {
    pub name: String,
    pub display_name: String,
    pub language: String,
}

/// A `jupyter server` Shame started; stopped when dropped.
pub struct LocalServer {
    child: Child,
    pub info: ServerInfo,
}

impl LocalServer {
    /// Start `jupyter server` for `root` on a free local port with a fresh
    /// token, and wait until it answers.
    pub fn start(jupyter: &Path, root: &Path) -> Result<Self, String> {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("no free port: {e}"))?;
        let token = random_token();
        let mut child = Command::new(jupyter)
            .arg("server")
            .arg("--no-browser")
            .arg("--ServerApp.ip=127.0.0.1")
            .arg(format!("--ServerApp.port={port}"))
            .arg("--ServerApp.port_retries=0")
            .arg(format!("--IdentityProvider.token={token}"))
            .arg(format!("--ServerApp.root_dir={}", root.display()))
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("couldn't start {}: {e}", jupyter.display()))?;
        // Keep the last lines of its log for a useful error.
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        if let Some(stderr) = child.stderr.take() {
            let log = log.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Ok(mut l) = log.lock() {
                        l.push(line);
                        if l.len() > 20 {
                            l.remove(0);
                        }
                    }
                }
            });
        }
        let info = ServerInfo { base_url: format!("http://127.0.0.1:{port}"), token: Some(token) };
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                let tail = log.lock().map(|l| l.join("\n")).unwrap_or_default();
                return Err(format!("jupyter server exited ({status}):\n{tail}"));
            }
            if info.status().is_ok() {
                return Ok(Self { child, info });
            }
            if started.elapsed() > Duration::from_secs(45) {
                let _ = child.kill();
                return Err("jupyter server didn't come up within 45 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 32 hex chars from the OS's random source.
pub(crate) fn random_token() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_err() {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes = n.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_urls() {
        let s = ServerInfo::parse("http://127.0.0.1:8888/lab?token=abc123").unwrap();
        assert_eq!(s, ServerInfo { base_url: "http://127.0.0.1:8888".into(), token: Some("abc123".into()) });
        assert!(s.is_local());

        let hub = ServerInfo::parse("https://hub.example.com/user/ada/tree?token=t&foo=1").unwrap();
        assert_eq!(hub.base_url, "https://hub.example.com/user/ada");
        assert_eq!(hub.token.as_deref(), Some("t"));
        assert!(!hub.is_local());

        let bare = ServerInfo::parse("https://pod-8888.proxy.runpod.net/").unwrap();
        assert_eq!(bare.base_url, "https://pod-8888.proxy.runpod.net");
        assert_eq!(bare.token, None);

        assert!(ServerInfo::parse("localhost:8888").is_err());
        let colab = ServerInfo::parse("https://colab.research.google.com/drive/1abc").unwrap_err();
        assert!(colab.contains("Colab"), "{colab}");
        assert!(is_colab_url("https://8080-m-s-abc.asia-east1-0.prod.colab.googleusercontent.com/"));
        assert!(!is_colab_url("https://hub.example.com/user/ada"));
        assert!(ServerInfo::parse("https://").is_err());
    }

    #[test]
    fn websocket_urls_follow_the_scheme() {
        let s = ServerInfo { base_url: "https://h/user/a".into(), token: Some("t".into()) };
        assert_eq!(s.channels_url("k1", "s1"), "wss://h/user/a/api/kernels/k1/channels?session_id=s1&token=t");
        let l = ServerInfo { base_url: "http://127.0.0.1:9".into(), token: None };
        assert_eq!(l.channels_url("k", "s"), "ws://127.0.0.1:9/api/kernels/k/channels?session_id=s");
        assert_eq!(random_token().len(), 32);
    }
}
