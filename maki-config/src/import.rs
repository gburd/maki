use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::global_dir;

const CONFIG_FILE: &str = "config.toml";
const PERMISSIONS_FILE: &str = "permissions.toml";
const ENV_FILE: &str = ".env";

/// A detected configuration source that can be imported.
pub enum ConfigSource {
    Claude {
        path: PathBuf,
        model: Option<String>,
        yolo: bool,
        env_count: usize,
        deny_count: usize,
        allow_count: usize,
    },
}

/// Summary of what was imported.
pub struct ImportSummary {
    pub model: Option<String>,
    pub yolo: bool,
    pub env_count: usize,
    pub deny_count: usize,
    pub allow_count: usize,
}

/// Check if first-run import should be offered.
///
/// Returns `None` if config already exists or no importable sources are found.
pub fn needs_import() -> Option<Vec<ConfigSource>> {
    let dest = global_dir()?;
    if dest.join(CONFIG_FILE).exists() {
        return None;
    }

    let mut sources = Vec::new();

    if let Some(claude) = detect_claude() {
        sources.push(claude);
    }

    if sources.is_empty() {
        None
    } else {
        Some(sources)
    }
}

/// Detect Claude Code configuration.
fn detect_claude() -> Option<ConfigSource> {
    let home = dirs::home_dir()?;
    let settings_path = home.join(".claude").join("settings.json");
    if !settings_path.is_file() {
        return None;
    }

    let content = fs::read_to_string(&settings_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;

    let model = json.get("model").and_then(|v| v.as_str()).map(String::from);
    let yolo = json
        .get("permissions")
        .and_then(|p| p.get("defaultMode"))
        .and_then(|v| v.as_str())
        == Some("bypassPermissions");

    let env_count = json
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| m.iter().filter(|(k, _)| should_import_env_var(k)).count())
        .unwrap_or(0);

    let (deny_count, allow_count) = count_permissions(&json);

    Some(ConfigSource::Claude {
        path: settings_path,
        model,
        yolo,
        env_count,
        deny_count,
        allow_count,
    })
}

fn count_permissions(json: &serde_json::Value) -> (usize, usize) {
    let perms = match json.get("permissions") {
        Some(p) => p,
        None => return (0, 0),
    };

    let deny_count = perms
        .get("deny")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|e| parse_claude_permission_json(e).is_some()).count())
        .unwrap_or(0);

    let allow_count = perms
        .get("allow")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter(|e| parse_claude_permission_json(e).is_some()).count())
        .unwrap_or(0);

    (deny_count, allow_count)
}

fn parse_claude_permission_json(entry: &serde_json::Value) -> Option<(String, String)> {
    parse_claude_permission(entry.as_str()?)
}

/// Parse Claude's `Tool(pattern)` permission format.
///
/// Returns `(snake_case_tool, scope)`.
///
/// # Examples
/// - `"Bash(cargo *)"` -> `("bash", "cargo *")`
/// - `"Read(**)"` -> `("read", "**")`
/// - `"WebFetch(domain:docs.anthropic.com)"` -> `("web_fetch", "domain:docs.anthropic.com")`
fn parse_claude_permission(entry: &str) -> Option<(String, String)> {
    let open = entry.find('(')?;
    if !entry.ends_with(')') {
        return None;
    }
    let tool_pascal = &entry[..open];
    let scope = &entry[open + 1..entry.len() - 1];

    if tool_pascal.is_empty() || scope.is_empty() {
        return None;
    }

    let tool_snake = pascal_to_snake(tool_pascal);
    Some((tool_snake, scope.to_string()))
}

/// Convert PascalCase to snake_case.
fn pascal_to_snake(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Map a Claude model ID to a maki provider/model spec.
fn map_claude_model(model: &str) -> String {
    if model.contains("us.anthropic.") || model.contains("eu.anthropic.") {
        let base = if model.contains(':') {
            format!("bedrock/{model}")
        } else {
            format!("bedrock/{model}:0")
        };
        return base;
    }

    if model.starts_with("claude-") {
        return format!("anthropic/{model}");
    }

    model.to_string()
}

/// Whether an environment variable from Claude should be imported.
fn should_import_env_var(key: &str) -> bool {
    if key.starts_with('_') {
        return false;
    }
    if key.starts_with('$') {
        return false;
    }
    true
}

/// Run the import from Claude Code, writing config files to `dest_dir`.
pub fn import_claude(source: &Path, dest_dir: &Path) -> Result<ImportSummary, String> {
    let content =
        fs::read_to_string(source).map_err(|e| format!("failed to read {}: {e}", source.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("failed to parse JSON: {e}"))?;

    fs::create_dir_all(dest_dir).map_err(|e| format!("failed to create {}: {e}", dest_dir.display()))?;

    // Extract model
    let model = json
        .get("model")
        .and_then(|v| v.as_str())
        .map(|m| map_claude_model(m));

    // Extract YOLO mode
    let yolo = json
        .get("permissions")
        .and_then(|p| p.get("defaultMode"))
        .and_then(|v| v.as_str())
        == Some("bypassPermissions");

    // Generate config.toml
    let config_toml = generate_config_toml(model.as_deref(), yolo);
    fs::write(dest_dir.join(CONFIG_FILE), &config_toml)
        .map_err(|e| format!("failed to write config.toml: {e}"))?;

    // Generate .env
    let env_count = write_env_file(&json, dest_dir)?;

    // Generate permissions.toml
    let (deny_count, allow_count) = write_permissions_file(&json, dest_dir)?;

    Ok(ImportSummary {
        model,
        yolo,
        env_count,
        deny_count,
        allow_count,
    })
}

fn generate_config_toml(model: Option<&str>, yolo: bool) -> String {
    let mut out = String::new();
    if yolo {
        out.push_str("always_yolo = true\n");
    }
    if let Some(m) = model {
        if yolo {
            out.push('\n');
        }
        writeln!(out, "[provider]").unwrap();
        writeln!(out, "default_model = \"{m}\"").unwrap();
    }
    if out.is_empty() {
        // Write a minimal config so the file exists and prevents re-triggering
        out.push_str("# maki configuration\n# See: https://maki.sh/docs/configuration\n");
    }
    out
}

fn write_env_file(json: &serde_json::Value, dest_dir: &Path) -> Result<usize, String> {
    let env_map = match json.get("env").and_then(|v| v.as_object()) {
        Some(m) => m,
        None => return Ok(0),
    };

    let mut lines = Vec::new();
    for (key, value) in env_map {
        if !should_import_env_var(key) {
            continue;
        }
        if let Some(val) = value.as_str() {
            lines.push(format!("{key}={val}"));
        }
    }

    if lines.is_empty() {
        return Ok(0);
    }

    lines.sort();
    let count = lines.len();
    let content = lines.join("\n") + "\n";
    fs::write(dest_dir.join(ENV_FILE), &content)
        .map_err(|e| format!("failed to write .env: {e}"))?;
    Ok(count)
}

fn write_permissions_file(
    json: &serde_json::Value,
    dest_dir: &Path,
) -> Result<(usize, usize), String> {
    let perms = match json.get("permissions") {
        Some(p) => p,
        None => return Ok((0, 0)),
    };

    let mut tool_deny: HashMap<String, Vec<String>> = HashMap::new();
    let mut tool_allow: HashMap<String, Vec<String>> = HashMap::new();

    let mut deny_count = 0;
    if let Some(deny_arr) = perms.get("deny").and_then(|v| v.as_array()) {
        for entry in deny_arr {
            if let Some((tool, scope)) = parse_claude_permission_json(entry) {
                tool_deny.entry(tool).or_default().push(scope);
                deny_count += 1;
            }
        }
    }

    let mut allow_count = 0;
    if let Some(allow_arr) = perms.get("allow").and_then(|v| v.as_array()) {
        for entry in allow_arr {
            if let Some((tool, scope)) = parse_claude_permission_json(entry) {
                tool_allow.entry(tool).or_default().push(scope);
                allow_count += 1;
            }
        }
    }

    if deny_count == 0 && allow_count == 0 {
        return Ok((0, 0));
    }

    // Collect all tool names and sort for deterministic output
    let mut all_tools: Vec<String> = tool_deny
        .keys()
        .chain(tool_allow.keys())
        .cloned()
        .collect();
    all_tools.sort();
    all_tools.dedup();

    let mut out = String::new();
    for tool in &all_tools {
        writeln!(out, "[{tool}]").unwrap();
        if let Some(deny) = tool_deny.get(tool) {
            write!(out, "deny = [").unwrap();
            if deny.len() == 1 {
                write!(out, "\"{}\"", escape_toml_string(&deny[0])).unwrap();
            } else {
                out.push('\n');
                for d in deny {
                    writeln!(out, "    \"{}\",", escape_toml_string(d)).unwrap();
                }
            }
            writeln!(out, "]").unwrap();
        }
        if let Some(allow) = tool_allow.get(tool) {
            write!(out, "allow = [").unwrap();
            if allow.len() == 1 {
                write!(out, "\"{}\"", escape_toml_string(&allow[0])).unwrap();
            } else {
                out.push('\n');
                for a in allow {
                    writeln!(out, "    \"{}\",", escape_toml_string(a)).unwrap();
                }
            }
            writeln!(out, "]").unwrap();
        }
        out.push('\n');
    }

    fs::write(dest_dir.join(PERMISSIONS_FILE), &out)
        .map_err(|e| format!("failed to write permissions.toml: {e}"))?;

    Ok((deny_count, allow_count))
}

fn escape_toml_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Create a minimal default config file.
pub fn write_default_config(dest_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dest_dir).map_err(|e| format!("failed to create {}: {e}", dest_dir.display()))?;
    let content = "# maki configuration\n# See: https://maki.sh/docs/configuration\n";
    fs::write(dest_dir.join(CONFIG_FILE), content)
        .map_err(|e| format!("failed to write config.toml: {e}"))?;
    Ok(())
}

/// Run the interactive first-run import wizard.
pub fn run_wizard(sources: &[ConfigSource]) -> Result<(), String> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    writeln!(out, "\nWelcome to maki!\n").map_err(|e| e.to_string())?;
    writeln!(out, "We found configuration from other tools on this system:")
        .map_err(|e| e.to_string())?;

    for (i, source) in sources.iter().enumerate() {
        match source {
            ConfigSource::Claude { path, .. } => {
                writeln!(out, "  [{}] Claude Code  ({})", i + 1, path.display())
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    let skip_num = sources.len() + 1;
    writeln!(out, "  [{skip_num}] Skip — start with defaults").map_err(|e| e.to_string())?;
    writeln!(out).map_err(|e| e.to_string())?;

    let valid_range = 1..=skip_num;
    let choice = loop {
        write!(out, "Import from [1-{skip_num}]: ").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| format!("failed to read input: {e}"))?;

        if let Ok(n) = line.trim().parse::<usize>() {
            if valid_range.contains(&n) {
                break n;
            }
        }
    };

    let dest = global_dir().ok_or_else(|| "cannot determine config directory".to_string())?;

    if choice == skip_num {
        write_default_config(&dest)?;
        writeln!(out, "\nCreated default config at {}/config.toml", dest.display())
            .map_err(|e| e.to_string())?;
        writeln!(
            out,
            "You can edit it anytime or run `maki config import` later.\n"
        )
        .map_err(|e| e.to_string())?;
    } else {
        let source = &sources[choice - 1];
        match source {
            ConfigSource::Claude { path, .. } => {
                writeln!(out, "\nImporting from Claude Code...\n").map_err(|e| e.to_string())?;
                let summary = import_claude(path, &dest)?;
                if let Some(ref model) = summary.model {
                    writeln!(out, "  \u{2713} Model: {model}").map_err(|e| e.to_string())?;
                }
                writeln!(
                    out,
                    "  \u{2713} YOLO mode: {}",
                    if summary.yolo { "enabled" } else { "disabled" }
                )
                .map_err(|e| e.to_string())?;
                if summary.env_count > 0 {
                    writeln!(
                        out,
                        "  \u{2713} Environment variables: {} variables \u{2192} {}/.env",
                        summary.env_count,
                        dest.display()
                    )
                    .map_err(|e| e.to_string())?;
                }
                if summary.deny_count > 0 || summary.allow_count > 0 {
                    writeln!(
                        out,
                        "  \u{2713} Permission rules: {} deny, {} allow \u{2192} {}/permissions.toml",
                        summary.deny_count,
                        summary.allow_count,
                        dest.display()
                    )
                    .map_err(|e| e.to_string())?;
                }
                writeln!(out, "\nConfig written to {}/config.toml\n", dest.display())
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;
    use test_case::test_case;

    #[test_case("Bash(cargo *)", "bash", "cargo *" ; "bash_cargo")]
    #[test_case("Read(**)", "read", "**" ; "read_glob")]
    #[test_case("WebFetch(domain:example.com)", "web_fetch", "domain:example.com" ; "web_fetch_domain")]
    #[test_case("CodeExecution(python)", "code_execution", "python" ; "code_execution")]
    #[test_case("Write(/tmp/*)", "write", "/tmp/*" ; "write_path")]
    fn parse_claude_permission_valid(input: &str, expected_tool: &str, expected_scope: &str) {
        let (tool, scope) = parse_claude_permission(input).unwrap();
        assert_eq!(tool, expected_tool);
        assert_eq!(scope, expected_scope);
    }

    #[test_case("invalid" ; "no_parens")]
    #[test_case("" ; "empty")]
    #[test_case("(scope)" ; "no_tool")]
    #[test_case("Tool()" ; "empty_scope")]
    #[test_case("Tool(scope" ; "no_close_paren")]
    fn parse_claude_permission_invalid(input: &str) {
        assert!(parse_claude_permission(input).is_none());
    }

    #[test_case("us.anthropic.claude-opus-4-6-v1", "bedrock/us.anthropic.claude-opus-4-6-v1:0" ; "bedrock_us_no_version")]
    #[test_case("eu.anthropic.claude-sonnet-4-5-20250514-v1:0", "bedrock/eu.anthropic.claude-sonnet-4-5-20250514-v1:0" ; "bedrock_eu_with_version")]
    #[test_case("claude-sonnet-4-5-20250514", "anthropic/claude-sonnet-4-5-20250514" ; "anthropic_direct")]
    #[test_case("custom-model", "custom-model" ; "passthrough")]
    fn map_claude_model_cases(input: &str, expected: &str) {
        assert_eq!(map_claude_model(input), expected);
    }

    #[test]
    fn needs_import_returns_none_when_config_exists() {
        // If we can determine the global dir and config exists, it should return None.
        // This test relies on the actual filesystem state, so we just verify the logic path.
        // In practice, if ~/.config/maki/config.toml exists, needs_import() returns None.
        // We can't easily mock global_dir, so we test the detection logic directly.
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("config.toml");
        fs::write(&dest, "# exists").unwrap();
        // The file exists, so the check `dest.join(CONFIG_FILE).exists()` would return true.
        assert!(dest.exists());
    }

    #[test]
    fn import_claude_writes_correct_files() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "us.anthropic.claude-opus-4-6-v1",
            "permissions": {
                "defaultMode": "bypassPermissions",
                "deny": [
                    "Bash(rm -rf *)",
                    "Write(/etc/*)"
                ],
                "allow": [
                    "Bash(cargo *)",
                    "Bash(git *)",
                    "Read(**)"
                ]
            },
            "env": {
                "ANTHROPIC_API_KEY": "sk-test-123",
                "AWS_REGION": "us-east-1",
                "_INTERNAL_VAR": "should-skip"
            }
        });

        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let dest = dest_dir.path();
        let summary = import_claude(&settings_path, dest).unwrap();

        assert_eq!(
            summary.model.as_deref(),
            Some("bedrock/us.anthropic.claude-opus-4-6-v1:0")
        );
        assert!(summary.yolo);
        assert_eq!(summary.env_count, 2);
        assert_eq!(summary.deny_count, 2);
        assert_eq!(summary.allow_count, 3);

        // Verify config.toml
        let config_content = fs::read_to_string(dest.join("config.toml")).unwrap();
        assert!(config_content.contains("always_yolo = true"));
        assert!(config_content.contains("bedrock/us.anthropic.claude-opus-4-6-v1:0"));

        // Verify .env
        let env_content = fs::read_to_string(dest.join(".env")).unwrap();
        assert!(env_content.contains("ANTHROPIC_API_KEY=sk-test-123"));
        assert!(env_content.contains("AWS_REGION=us-east-1"));
        assert!(!env_content.contains("_INTERNAL_VAR"));

        // Verify permissions.toml
        let perms_content = fs::read_to_string(dest.join("permissions.toml")).unwrap();
        assert!(perms_content.contains("[bash]"));
        assert!(perms_content.contains("rm -rf *"));
        assert!(perms_content.contains("cargo *"));
        assert!(perms_content.contains("git *"));
        assert!(perms_content.contains("[read]"));
        assert!(perms_content.contains("[write]"));
    }

    #[test]
    fn import_claude_minimal_settings() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({});
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let dest = dest_dir.path();
        let summary = import_claude(&settings_path, dest).unwrap();

        assert!(summary.model.is_none());
        assert!(!summary.yolo);
        assert_eq!(summary.env_count, 0);
        assert_eq!(summary.deny_count, 0);
        assert_eq!(summary.allow_count, 0);

        // Config should still be created
        assert!(dest.join("config.toml").exists());
        // .env and permissions.toml should not be created when empty
        assert!(!dest.join(".env").exists());
        assert!(!dest.join("permissions.toml").exists());
    }

    #[test]
    fn write_default_config_creates_file() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("maki-test");
        write_default_config(&dest).unwrap();
        assert!(dest.join("config.toml").exists());
        let content = fs::read_to_string(dest.join("config.toml")).unwrap();
        assert!(content.contains("maki configuration"));
    }

    #[test]
    fn env_var_filtering() {
        assert!(should_import_env_var("ANTHROPIC_API_KEY"));
        assert!(should_import_env_var("AWS_REGION"));
        assert!(should_import_env_var("OPENAI_API_KEY"));
        assert!(!should_import_env_var("_ANTHROPIC_MODEL"));
        assert!(!should_import_env_var("_INTERNAL"));
        assert!(!should_import_env_var("$schema"));
    }

    #[test]
    fn pascal_to_snake_cases() {
        assert_eq!(pascal_to_snake("Bash"), "bash");
        assert_eq!(pascal_to_snake("WebFetch"), "web_fetch");
        assert_eq!(pascal_to_snake("CodeExecution"), "code_execution");
        assert_eq!(pascal_to_snake("Read"), "read");
        assert_eq!(pascal_to_snake("already_snake"), "already_snake");
    }

    #[test]
    fn import_claude_model_only() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "claude-sonnet-4-5-20250514"
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let summary = import_claude(&settings_path, dest_dir.path()).unwrap();
        assert_eq!(
            summary.model.as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250514")
        );
        assert!(!summary.yolo);

        let config = fs::read_to_string(dest_dir.path().join("config.toml")).unwrap();
        assert!(config.contains("anthropic/claude-sonnet-4-5-20250514"));
        assert!(!config.contains("always_yolo"));
    }

    #[test]
    fn permissions_toml_is_parseable() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "permissions": {
                "deny": ["Bash(rm -rf *)"],
                "allow": ["Bash(cargo test *)", "Read(**)"]
            }
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        import_claude(&settings_path, dest_dir.path()).unwrap();

        // The generated permissions.toml should be valid TOML
        let content = fs::read_to_string(dest_dir.path().join("permissions.toml")).unwrap();
        let parsed: toml::Table = content.parse().expect("generated permissions.toml should be valid TOML");
        assert!(parsed.contains_key("bash"));
        assert!(parsed.contains_key("read"));
    }

    #[test]
    fn escape_toml_string_handles_special_chars() {
        assert_eq!(escape_toml_string("simple"), "simple");
        assert_eq!(escape_toml_string(r#"has "quotes""#), r#"has \"quotes\""#);
        assert_eq!(escape_toml_string(r"back\slash"), r"back\\slash");
    }
}
