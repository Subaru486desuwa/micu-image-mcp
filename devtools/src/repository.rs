use crate::Result;
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    process::Command,
};

pub fn source_audit(root: &Path) -> Result<Value> {
    let files = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()?;
    if !files.status.success() {
        return Err("source audit requires a Git checkout".into());
    }
    let mut violations = Vec::new();
    let mut examined = 0;
    for bytes in files
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(bytes)?;
        let path = root.join(relative);
        if !path.is_file() {
            continue;
        }
        examined += 1;
        if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("py" | "pyi" | "pyc" | "ts" | "tsx" | "js" | "mjs" | "cjs")
        ) || matches!(
            path.file_name().and_then(|value| value.to_str()),
            Some("pyproject.toml" | "setup.py" | "setup.cfg" | "Pipfile" | "poetry.lock")
        ) {
            violations.push(relative.to_owned());
            continue;
        }
        let executable_text = relative.starts_with(".github/workflows/")
            || matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("sh" | "ps1" | "rs")
            );
        if !executable_text {
            continue;
        }
        let contents = fs::read_to_string(&path)?;
        let suspicious = if path.extension().is_some_and(|extension| extension == "rs") {
            contents.contains("Command::new(\"python") || contents.contains("var_os(\"PYTHON\")")
        } else {
            [
                "actions/setup-python",
                "shell: python",
                "python -",
                "python3 ",
                ".venv/bin/python",
                "MICU_MCP_PYTHON",
                "actions/github-script",
                "script: |",
            ]
            .iter()
            .any(|pattern| contents.contains(pattern))
        };
        if suspicious {
            violations.push(relative.to_owned());
        }
    }
    violations.sort();
    violations.dedup();
    if !violations.is_empty() {
        return Err(format!(
            "non-Rust executable source or interpreter dependency: {}",
            violations.join(", ")
        )
        .into());
    }
    Ok(json!({"passed":true,"examined_files":examined,"python_files":0,"inline_interpreters":0}))
}

pub fn release_check(
    root: &Path,
    tag: Option<&str>,
    github_output: Option<&Path>,
) -> Result<Value> {
    let package: toml_edit::DocumentMut = fs::read_to_string(root.join("Cargo.toml"))?.parse()?;
    if package["package"]["name"].as_str() != Some("micu-image-mcp")
        || !root.join("src/main.rs").is_file()
    {
        return Err("only the root Rust package may be released".into());
    }
    let version = package["package"]["version"]
        .as_str()
        .ok_or("package.version missing")?;
    let mut notes = String::new();
    let mut prerelease = false;
    if let Some(tag) = tag {
        let parsed = semver::Version::parse(
            tag.strip_prefix('v')
                .ok_or("release tag must start with v")?,
        )?;
        if !parsed.build.is_empty() || tag != format!("v{version}") {
            return Err("release tag must match package.version without build metadata".into());
        }
        notes = format!("docs/releases/{tag}.md");
        if !root.join(&notes).is_file() {
            return Err("version-specific release notes missing".into());
        }
        prerelease = !parsed.pre.is_empty();
    }
    if let Some(output) = github_output {
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(output)?;
        writeln!(file, "notes={notes}\nprerelease={prerelease}")?;
    }
    Ok(json!({"package":"micu-image-mcp","version":version,"notes":notes,"prerelease":prerelease}))
}

struct Github {
    client: reqwest::blocking::Client,
    base: String,
    token: String,
}

impl Github {
    fn request(&self, method: reqwest::Method, path: &str) -> Result<Option<Value>> {
        let mut authorization =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {}", self.token))
                .map_err(|_| "invalid GitHub token")?;
        authorization.set_sensitive(true);
        let response = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header(reqwest::header::AUTHORIZATION, authorization)
            .send()
            .map_err(|_| "GitHub API transport failed")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(
                format!("GitHub API rejected operation: HTTP {}", response.status()).into(),
            );
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(Some(json!({})));
        }
        let mut bytes = Vec::new();
        response
            .take(5 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "reading GitHub response failed")?;
        if bytes.len() > 5 * 1024 * 1024 {
            return Err("GitHub response exceeds limit".into());
        }
        Ok(Some(
            serde_json::from_slice(&bytes).map_err(|_| "invalid GitHub JSON")?,
        ))
    }
    fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path)?
            .ok_or_else(|| "GitHub object missing".into())
    }
    fn open_pulls(&self) -> Result<Vec<Value>> {
        let mut pulls = Vec::new();
        for page in 1..=100 {
            let value = self.get(&format!("/pulls?state=open&per_page=100&page={page}"))?;
            let chunk = value.as_array().ok_or("invalid GitHub pull list")?;
            pulls.extend(chunk.iter().cloned());
            if chunk.len() < 100 {
                return Ok(pulls);
            }
        }
        Err("too many open pull request pages; refusing incomplete audit".into())
    }
    fn ancestor(&self, sha: &str, main: &str) -> Result<()> {
        let comparison = self.get(&format!("/compare/{sha}...{main}"))?;
        if !matches!(comparison["status"].as_str(), Some("ahead" | "identical")) {
            return Err("branch history is not retained on main".into());
        }
        Ok(())
    }
}

pub fn maintain_branches(repo: &str, event: Option<&Path>, apply: bool) -> Result<Value> {
    if repo != "Subaru486desuwa/micu-image-mcp" {
        return Err("branch maintenance is restricted to the configured repository".into());
    }
    let api = Github {
        client: reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .no_proxy()
            .user_agent("micu-rust-branch-maintenance")
            .build()?,
        base: format!("https://api.github.com/repos/{repo}"),
        token: std::env::var("GITHUB_TOKEN").map_err(|_| "GITHUB_TOKEN is required")?,
    };
    if api.get("")?["default_branch"] != "main" {
        return Err("default branch must remain main".into());
    }
    let mut retired: Vec<(String, String)> = Vec::new();
    if let Some(event) = event {
        let value: Value = serde_json::from_slice(&fs::read(event)?)?;
        let pr = &value["pull_request"];
        let name = pr["head"]["ref"].as_str().unwrap_or_default();
        if pr["merged"] == true
            && pr["base"]["ref"] == "main"
            && pr["head"]["repo"]["full_name"] == repo
            && name.starts_with("codex/")
        {
            retired.push((
                name.to_owned(),
                pr["head"]["sha"]
                    .as_str()
                    .ok_or("pull request tip missing")?
                    .to_owned(),
            ));
        }
    } else {
        for (name, sha) in [
            (
                "codex/add-diving-kitten",
                "f5f04773e868206e96e10c3c7a6e8b6f11903e5f",
            ),
            (
                "codex/line-compat-update",
                "eb743099a66836f1a1ec3d848f5164f1b9958105",
            ),
            ("rust-port", "bfda7956e0dd7c4ac3d55ce7ae7f6d845f191f6b"),
            ("ts-port", "192d0b362de0df47478fc31cbfb8199761cc5e8c"),
        ] {
            retired.push((name.into(), sha.into()));
        }
    }
    let pulls = api.open_pulls()?;
    let mut eligible = Vec::new();
    let mut preserved = Vec::new();
    for (name, sha) in retired {
        let Some(reference) =
            api.request(reqwest::Method::GET, &format!("/git/ref/heads/{name}"))?
        else {
            continue;
        };
        let branch = api.get(&format!("/branches/{}", name.replace('/', "%2F")))?;
        let in_use = pulls.iter().any(|pr| {
            pr["base"]["ref"] == name
                || (pr["head"]["repo"]["full_name"] == repo && pr["head"]["ref"] == name)
        });
        if reference["object"]["sha"] != sha || branch["protected"] == true || in_use {
            preserved.push(name);
            continue;
        }
        let main = api.get("/git/ref/heads/main")?;
        let main = main["object"]["sha"].as_str().ok_or("main tip missing")?;
        if api.ancestor(&sha, main).is_err() {
            preserved.push(name);
            continue;
        }
        if name == "ts-port" {
            api.ancestor("0bae340e8d5ab01a906f3dac2b4234787e1841b5", main)?;
        }
        eligible.push((name, sha));
    }
    // Validate the entire plan before mutating refs, then recheck each ref at deletion.
    let mut removed = Vec::new();
    for (name, sha) in eligible {
        if apply {
            let latest = api.get(&format!("/git/ref/heads/{name}"))?;
            if latest["object"]["sha"] != sha {
                return Err("branch moved during cleanup".into());
            }
            let branch = api.get(&format!("/branches/{}", name.replace('/', "%2F")))?;
            let pulls = api.open_pulls()?;
            if branch["protected"] == true
                || pulls.iter().any(|pr| {
                    pr["base"]["ref"] == name
                        || (pr["head"]["repo"]["full_name"] == repo && pr["head"]["ref"] == name)
                })
            {
                return Err("branch became protected or in use during cleanup".into());
            }
            let main = api.get("/git/ref/heads/main")?;
            let main = main["object"]["sha"].as_str().ok_or("main tip missing")?;
            api.ancestor(&sha, main)?;
            if name == "ts-port" {
                api.ancestor("0bae340e8d5ab01a906f3dac2b4234787e1841b5", main)?;
            }
            api.request(reqwest::Method::DELETE, &format!("/git/refs/heads/{name}"))?;
        }
        removed.push(name);
    }
    Ok(json!({"apply":apply,"eligible_or_removed":removed,"preserved":preserved}))
}
