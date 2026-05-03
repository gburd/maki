use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::global_dir;

const INIT_LUA: &str = "init.lua";
const CONFIG_FILE: &str = "config.toml"; // legacy sentinel only — no longer written
const PERMISSIONS_FILE: &str = "permissions.toml";
const ENV_FILE: &str = ".env";

/// Parsed contents of a Claude settings.json, ready for selective import.
pub struct ClaudeConfig {
    pub path: PathBuf,
    pub model: Option<String>,
    pub mapped_model: Option<String>,
    pub yolo: bool,
    pub env_vars: Vec<(String, String)>,
    pub deny_rules: Vec<(String, String)>,
    pub allow_rules: Vec<(String, String)>,
}

impl ClaudeConfig {
    /// Returns true if there is at least one importable category.
    fn has_content(&self) -> bool {
        self.model.is_some()
            || self.yolo
            || !self.env_vars.is_empty()
            || !self.deny_rules.is_empty()
            || !self.allow_rules.is_empty()
    }
}

/// Whether an environment variable from Claude should be imported.
///
/// Skips Claude Code internal vars that are useless to maki:
/// - `_`-prefixed (internal markers like `_ANTHROPIC_MODEL`)
/// - `$`-prefixed (schema markers)
/// - `CLAUDE_CODE_*` (application settings — maki infers provider from model string)
/// - `DISABLE_*` (telemetry/feature flags)
fn should_import_env_var(key: &str) -> bool {
    if key.starts_with('_') || key.starts_with('$') {
        return false;
    }
    if key.starts_with("CLAUDE_CODE_") {
        return false;
    }
    if key.starts_with("DISABLE_") {
        return false;
    }
    true
}

/// Parse Claude settings.json into importable categories.
pub fn parse_claude_config(path: &Path) -> Result<ClaudeConfig, String> {
    let content =
        fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let model = json.get("model").and_then(|v| v.as_str()).map(String::from);
    let mapped_model = model.as_deref().map(map_claude_model);

    let yolo = json
        .get("permissions")
        .and_then(|p| p.get("defaultMode"))
        .and_then(|v| v.as_str())
        == Some("bypassPermissions");

    let mut env_vars: Vec<(String, String)> = json
        .get("env")
        .and_then(|v| v.as_object())
        .map(|m| {
            m.iter()
                .filter(|(k, _)| should_import_env_var(k))
                .filter_map(|(k, v)| v.as_str().map(|val| (k.clone(), val.to_string())))
                .collect()
        })
        .unwrap_or_default();
    env_vars.sort_by(|a, b| a.0.cmp(&b.0));

    let perms = json.get("permissions");

    let deny_rules: Vec<(String, String)> = perms
        .and_then(|p| p.get("deny"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|e| parse_claude_permission_json(e)).collect())
        .unwrap_or_default();

    let allow_rules: Vec<(String, String)> = perms
        .and_then(|p| p.get("allow"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|e| parse_claude_permission_json(e)).collect())
        .unwrap_or_default();

    Ok(ClaudeConfig {
        path: path.to_path_buf(),
        model,
        mapped_model,
        yolo,
        env_vars,
        deny_rules,
        allow_rules,
    })
}

/// Ask y/n for a single category. Returns true on Y/enter, false on n.
fn prompt_yes_no(reader: &mut impl BufRead, out: &mut impl Write) -> Result<bool, String> {
    write!(out, "  Import? [Y/n]: ").map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|e| format!("failed to read input: {e}"))?;
    let answer = line.trim().to_lowercase();
    Ok(answer.is_empty() || answer == "y" || answer == "yes")
}

/// Run the interactive first-run import wizard.
///
/// Returns the imported model spec if a model was imported, or `None` if skipped
/// or config already exists.
pub fn run_wizard() -> Result<Option<String>, String> {
    let dest = global_dir().ok_or_else(|| "cannot determine config directory".to_string())?;

    // Skip if already configured — check both new (init.lua) and legacy (config.toml)
    if dest.join(INIT_LUA).exists() || dest.join(CONFIG_FILE).exists() {
        return Ok(None);
    }

    let settings_path = etcetera::home_dir()
        .ok()
        .map(|h| h.join(".claude").join("settings.json"));

    let config = settings_path
        .as_ref()
        .filter(|p| p.is_file())
        .map(|p| parse_claude_config(p))
        .transpose()?
        .filter(|c| c.has_content());

    let config = match config {
        Some(c) => c,
        None => {
            write_default_config(&dest)?;
            return Ok(None);
        }
    };

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    run_wizard_inner(&config, &dest, &mut reader, &mut out)
}

/// Inner wizard logic, parameterized for testing.
fn run_wizard_inner(
    config: &ClaudeConfig,
    dest: &Path,
    reader: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Option<String>, String> {
    writeln!(out, "\nWelcome to maki!\n").map_err(|e| e.to_string())?;
    writeln!(out, "Found Claude Code config at {}\n", config.path.display())
        .map_err(|e| e.to_string())?;

    let mut import_model = false;
    let mut import_yolo = false;
    let mut import_env = false;
    let mut import_perms = false;

    // Prompt for model
    if let (Some(raw_model), Some(mapped)) = (&config.model, &config.mapped_model) {
        writeln!(out, "  Model: {raw_model}").map_err(|e| e.to_string())?;
        writeln!(out, "    -> {mapped}").map_err(|e| e.to_string())?;
        import_model = prompt_yes_no(reader, out)?;
        writeln!(out).map_err(|e| e.to_string())?;
    }

    // Prompt for YOLO mode
    if config.yolo {
        writeln!(
            out,
            "  YOLO mode (bypass permission prompts): enabled in Claude"
        )
        .map_err(|e| e.to_string())?;
        import_yolo = prompt_yes_no(reader, out)?;
        writeln!(out).map_err(|e| e.to_string())?;
    }

    // Prompt for env vars
    if !config.env_vars.is_empty() {
        let count = config.env_vars.len();
        let var_names: Vec<&str> = config.env_vars.iter().map(|(k, _)| k.as_str()).collect();
        writeln!(
            out,
            "  Environment variables ({count} variable{}):",
            if count == 1 { "" } else { "s" }
        )
        .map_err(|e| e.to_string())?;
        writeln!(out, "    {}", var_names.join(", ")).map_err(|e| e.to_string())?;
        import_env = prompt_yes_no(reader, out)?;
        writeln!(out).map_err(|e| e.to_string())?;
    }

    // Prompt for permissions
    if !config.deny_rules.is_empty() || !config.allow_rules.is_empty() {
        writeln!(
            out,
            "  Permission rules: {} deny, {} allow",
            config.deny_rules.len(),
            config.allow_rules.len()
        )
        .map_err(|e| e.to_string())?;
        import_perms = prompt_yes_no(reader, out)?;
        writeln!(out).map_err(|e| e.to_string())?;
    }

    let any_selected = import_model || import_yolo || import_env || import_perms;

    fs::create_dir_all(dest)
        .map_err(|e| format!("failed to create {}: {e}", dest.display()))?;

    if any_selected {
        let model_for_config = if import_model {
            config.mapped_model.as_deref()
        } else {
            None
        };
        let yolo_for_config = import_yolo;

        let init_lua = generate_init_lua(model_for_config, yolo_for_config);
        fs::write(dest.join(INIT_LUA), &init_lua)
            .map_err(|e| format!("failed to write init.lua: {e}"))?;

        writeln!(out, "Importing...").map_err(|e| e.to_string())?;

        if import_model {
            if let Some(ref mapped) = config.mapped_model {
                writeln!(out, "  \u{2713} Model: {mapped}").map_err(|e| e.to_string())?;
            }
        }

        if import_yolo {
            writeln!(out, "  \u{2713} YOLO mode: enabled").map_err(|e| e.to_string())?;
        }

        if import_env {
            let count = write_env_file_from_vars(&config.env_vars, dest)?;
            if count > 0 {
                writeln!(
                    out,
                    "  \u{2713} Environment variables: {count} variable{} -> {}/.env",
                    if count == 1 { "" } else { "s" },
                    dest.display()
                )
                .map_err(|e| e.to_string())?;
            }
        }

        if import_perms {
            let (deny_count, allow_count) =
                write_permissions_file_from_rules(&config.deny_rules, &config.allow_rules, dest)?;
            if deny_count > 0 || allow_count > 0 {
                writeln!(
                    out,
                    "  \u{2713} Permission rules: {deny_count} deny, {allow_count} allow -> {}/permissions.toml",
                    dest.display()
                )
                .map_err(|e| e.to_string())?;
            }
        }

        writeln!(out, "\nConfig written to {}/init.lua\n", dest.display())
            .map_err(|e| e.to_string())?;

        let imported_model = if import_model {
            config.mapped_model.clone()
        } else {
            None
        };
        Ok(imported_model)
    } else {
        write_default_config(dest)?;
        writeln!(out, "Created default config at {}/init.lua", dest.display())
            .map_err(|e| e.to_string())?;
        writeln!(
            out,
            "You can edit it anytime or run `maki config import` later.\n"
        )
        .map_err(|e| e.to_string())?;
        Ok(None)
    }
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
///
/// Handles consecutive uppercase letters as a single word:
/// `LS` → `ls`, `XMLParser` → `xml_parser`, `WebFetch` → `web_fetch`.
fn pascal_to_snake(s: &str) -> String {
    let mut result = String::with_capacity(s.len() + 4);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                let prev = chars[i - 1];
                let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
                // Insert underscore before uppercase if preceded by lowercase,
                // or if preceded by uppercase and followed by lowercase
                // (e.g., "XMLParser" -> "xml_parser", "LS" -> "ls")
                if prev.is_ascii_lowercase() || (prev.is_ascii_uppercase() && next_is_lower) {
                    result.push('_');
                }
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
        return format!("bedrock/{model}");
    }

    if model.starts_with("claude-") {
        return format!("anthropic/{model}");
    }

    model.to_string()
}

fn generate_init_lua(model: Option<&str>, yolo: bool) -> String {
    if model.is_none() && !yolo {
        return "-- maki configuration\n-- See: https://maki.sh/docs/configuration\nmaki.setup({})\n"
            .into();
    }
    let mut fields = String::new();
    if yolo {
        fields.push_str("    always_yolo = true,\n");
    }
    if let Some(m) = model {
        fields.push_str("    provider = {\n");
        fields.push_str(&format!("        default_model = \"{m}\",\n"));
        fields.push_str("    },\n");
    }
    format!("maki.setup({{\n{fields}}})\n")
}

fn write_env_file_from_vars(vars: &[(String, String)], dest_dir: &Path) -> Result<usize, String> {
    if vars.is_empty() {
        return Ok(0);
    }

    let lines: Vec<String> = vars
        .iter()
        .map(|(k, v)| {
            let escaped = v.replace('\\', "\\\\").replace('"', "\\\"");
            format!("{k}=\"{escaped}\"")
        })
        .collect();
    let count = lines.len();
    let content = lines.join("\n") + "\n";
    fs::write(dest_dir.join(ENV_FILE), &content)
        .map_err(|e| format!("failed to write .env: {e}"))?;
    Ok(count)
}

fn write_permissions_file_from_rules(
    deny_rules: &[(String, String)],
    allow_rules: &[(String, String)],
    dest_dir: &Path,
) -> Result<(usize, usize), String> {
    if deny_rules.is_empty() && allow_rules.is_empty() {
        return Ok((0, 0));
    }

    let mut tool_deny: HashMap<String, Vec<String>> = HashMap::new();
    let mut tool_allow: HashMap<String, Vec<String>> = HashMap::new();

    for (tool, scope) in deny_rules {
        tool_deny.entry(tool.clone()).or_default().push(scope.clone());
    }
    for (tool, scope) in allow_rules {
        tool_allow.entry(tool.clone()).or_default().push(scope.clone());
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

    Ok((deny_rules.len(), allow_rules.len()))
}

fn escape_toml_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Create a minimal default config file.
pub fn write_default_config(dest_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dest_dir)
        .map_err(|e| format!("failed to create {}: {e}", dest_dir.display()))?;
    let content =
        "-- maki configuration\n-- See: https://maki.sh/docs/configuration\nmaki.setup({})\n";
    fs::write(dest_dir.join(INIT_LUA), content)
        .map_err(|e| format!("failed to write init.lua: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
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

    #[test_case("us.anthropic.claude-opus-4-6-v1", "bedrock/us.anthropic.claude-opus-4-6-v1" ; "bedrock_us")]
    #[test_case("eu.anthropic.claude-sonnet-4-5-20250514-v1:0", "bedrock/eu.anthropic.claude-sonnet-4-5-20250514-v1:0" ; "bedrock_eu_with_version")]
    #[test_case("claude-sonnet-4-5-20250514", "anthropic/claude-sonnet-4-5-20250514" ; "anthropic_direct")]
    #[test_case("custom-model", "custom-model" ; "passthrough")]
    fn map_claude_model_cases(input: &str, expected: &str) {
        assert_eq!(map_claude_model(input), expected);
    }

    #[test]
    fn env_var_filtering() {
        // Should import
        assert!(should_import_env_var("ANTHROPIC_API_KEY"));
        assert!(should_import_env_var("AWS_REGION"));
        assert!(should_import_env_var("AWS_BEARER_TOKEN_BEDROCK"));
        assert!(should_import_env_var("OPENAI_API_KEY"));

        // Should skip: underscore/dollar prefixed
        assert!(!should_import_env_var("_ANTHROPIC_MODEL"));
        assert!(!should_import_env_var("_INTERNAL"));
        assert!(!should_import_env_var("$schema"));

        // Should skip: CLAUDE_CODE_* (all of them)
        assert!(!should_import_env_var("CLAUDE_CODE_USE_BEDROCK"));
        assert!(!should_import_env_var("CLAUDE_CODE_MAX_TOKENS"));
        assert!(!should_import_env_var("CLAUDE_CODE_SOMETHING"));

        // Should skip: DISABLE_*
        assert!(!should_import_env_var("DISABLE_TELEMETRY"));
        assert!(!should_import_env_var("DISABLE_ERROR_REPORTING"));
        assert!(!should_import_env_var("DISABLE_BUG_COMMAND"));
        assert!(!should_import_env_var("DISABLE_PROMPT_CACHING"));
    }

    #[test]
    fn parse_claude_config_full() {
        let dir = TempDir::new().unwrap();
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
                "_INTERNAL_VAR": "should-skip",
                "CLAUDE_CODE_USE_BEDROCK": "1",
                "DISABLE_TELEMETRY": "1"
            }
        });

        let path = dir.path().join("settings.json");
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&path).unwrap();

        assert_eq!(config.model.as_deref(), Some("us.anthropic.claude-opus-4-6-v1"));
        assert_eq!(
            config.mapped_model.as_deref(),
            Some("bedrock/us.anthropic.claude-opus-4-6-v1")
        );
        assert!(config.yolo);
        // Only AWS_REGION and ANTHROPIC_API_KEY should pass the filter
        assert_eq!(config.env_vars.len(), 2);
        assert_eq!(config.env_vars[0].0, "ANTHROPIC_API_KEY");
        assert_eq!(config.env_vars[1].0, "AWS_REGION");
        assert_eq!(config.deny_rules.len(), 2);
        assert_eq!(config.allow_rules.len(), 3);
    }

    #[test]
    fn parse_claude_config_empty() {
        let dir = TempDir::new().unwrap();
        let settings = serde_json::json!({});
        let path = dir.path().join("settings.json");
        fs::write(&path, serde_json::to_string(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&path).unwrap();

        assert!(config.model.is_none());
        assert!(config.mapped_model.is_none());
        assert!(!config.yolo);
        assert!(config.env_vars.is_empty());
        assert!(config.deny_rules.is_empty());
        assert!(config.allow_rules.is_empty());
        assert!(!config.has_content());
    }

    #[test]
    fn prompt_yes_no_accepts_enter() {
        let input = b"\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();
        assert!(prompt_yes_no(&mut reader, &mut output).unwrap());
    }

    #[test]
    fn prompt_yes_no_accepts_y() {
        let input = b"y\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();
        assert!(prompt_yes_no(&mut reader, &mut output).unwrap());
    }

    #[test]
    fn prompt_yes_no_accepts_yes() {
        let input = b"YES\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();
        assert!(prompt_yes_no(&mut reader, &mut output).unwrap());
    }

    #[test]
    fn prompt_yes_no_rejects_n() {
        let input = b"n\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();
        assert!(!prompt_yes_no(&mut reader, &mut output).unwrap());
    }

    #[test]
    fn prompt_yes_no_rejects_no() {
        let input = b"No\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();
        assert!(!prompt_yes_no(&mut reader, &mut output).unwrap());
    }

    #[test]
    fn wizard_accepts_all_categories() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "us.anthropic.claude-opus-4-6-v1",
            "permissions": {
                "defaultMode": "bypassPermissions",
                "deny": ["Bash(rm -rf *)"],
                "allow": ["Bash(cargo *)", "Read(**)"]
            },
            "env": {
                "AWS_REGION": "us-east-1",
                "ANTHROPIC_API_KEY": "sk-123"
            }
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&settings_path).unwrap();

        // Simulate pressing Enter (Y) for all 4 categories
        let input = b"\n\n\n\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();

        let result = run_wizard_inner(&config, dest_dir.path(), &mut reader, &mut output).unwrap();
        assert_eq!(
            result.as_deref(),
            Some("bedrock/us.anthropic.claude-opus-4-6-v1")
        );

        // Check files were created
        assert!(dest_dir.path().join("init.lua").exists());
        assert!(dest_dir.path().join(".env").exists());
        assert!(dest_dir.path().join("permissions.toml").exists());

        // Verify init.lua
        let config_content = fs::read_to_string(dest_dir.path().join("init.lua")).unwrap();
        assert!(config_content.contains("always_yolo = true"));
        assert!(config_content.contains("bedrock/us.anthropic.claude-opus-4-6-v1"));

        // Verify .env (values are double-quoted)
        let env_content = fs::read_to_string(dest_dir.path().join(".env")).unwrap();
        assert!(env_content.contains(r#"ANTHROPIC_API_KEY="sk-123""#));
        assert!(env_content.contains(r#"AWS_REGION="us-east-1""#));

        // Verify permissions.toml
        let perms_content = fs::read_to_string(dest_dir.path().join("permissions.toml")).unwrap();
        assert!(perms_content.contains("[bash]"));
        assert!(perms_content.contains("[read]"));
    }

    #[test]
    fn wizard_skips_all_categories() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "us.anthropic.claude-opus-4-6-v1",
            "permissions": {
                "defaultMode": "bypassPermissions",
                "deny": ["Bash(rm -rf *)"],
                "allow": ["Bash(cargo *)"]
            },
            "env": {
                "AWS_REGION": "us-east-1"
            }
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&settings_path).unwrap();

        // Simulate typing 'n' for all 4 categories
        let input = b"n\nn\nn\nn\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();

        let result = run_wizard_inner(&config, dest_dir.path(), &mut reader, &mut output).unwrap();
        assert!(result.is_none());

        // init.lua should exist (default)
        assert!(dest_dir.path().join("init.lua").exists());
        // .env and permissions.toml should NOT exist
        assert!(!dest_dir.path().join(".env").exists());
        assert!(!dest_dir.path().join("permissions.toml").exists());

        let config_content = fs::read_to_string(dest_dir.path().join("init.lua")).unwrap();
        assert!(config_content.contains("maki configuration"));
        assert!(!config_content.contains("always_yolo"));
    }

    #[test]
    fn wizard_selective_import() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "claude-sonnet-4-5-20250514",
            "permissions": {
                "defaultMode": "bypassPermissions",
                "deny": ["Bash(rm -rf *)"],
                "allow": ["Bash(cargo *)"]
            },
            "env": {
                "AWS_REGION": "us-east-1"
            }
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&settings_path).unwrap();

        // Accept model, skip yolo, accept env, skip perms
        let input = b"y\nn\ny\nn\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();

        let result = run_wizard_inner(&config, dest_dir.path(), &mut reader, &mut output).unwrap();
        assert_eq!(
            result.as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250514")
        );

        let config_content = fs::read_to_string(dest_dir.path().join("init.lua")).unwrap();
        assert!(config_content.contains("anthropic/claude-sonnet-4-5-20250514"));
        assert!(!config_content.contains("always_yolo"));

        assert!(dest_dir.path().join(".env").exists());
        assert!(!dest_dir.path().join("permissions.toml").exists());
    }

    #[test]
    fn wizard_model_only() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "model": "claude-sonnet-4-5-20250514"
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(&settings_path, serde_json::to_string(&settings).unwrap()).unwrap();

        let config = parse_claude_config(&settings_path).unwrap();
        assert!(config.has_content());
        assert!(!config.yolo);
        assert!(config.env_vars.is_empty());

        // Only model prompt shown, accept it
        let input = b"\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();

        let result = run_wizard_inner(&config, dest_dir.path(), &mut reader, &mut output).unwrap();
        assert_eq!(
            result.as_deref(),
            Some("anthropic/claude-sonnet-4-5-20250514")
        );

        let config_content = fs::read_to_string(dest_dir.path().join("init.lua")).unwrap();
        assert!(config_content.contains("anthropic/claude-sonnet-4-5-20250514"));
        assert!(!config_content.contains("always_yolo"));
    }

    #[test]
    fn write_default_config_creates_file() {
        let dir = TempDir::new().unwrap();
        let dest = dir.path().join("maki-test");
        write_default_config(&dest).unwrap();
        assert!(dest.join("init.lua").exists());
        let content = fs::read_to_string(dest.join("init.lua")).unwrap();
        assert!(content.contains("maki configuration"));
    }

    #[test]
    fn permissions_toml_is_parseable() {
        let dest_dir = TempDir::new().unwrap();

        let deny_rules = vec![("bash".to_string(), "rm -rf *".to_string())];
        let allow_rules = vec![
            ("bash".to_string(), "cargo test *".to_string()),
            ("read".to_string(), "**".to_string()),
        ];

        write_permissions_file_from_rules(&deny_rules, &allow_rules, dest_dir.path()).unwrap();

        let content = fs::read_to_string(dest_dir.path().join("permissions.toml")).unwrap();
        let parsed: toml::Table =
            content.parse().expect("generated permissions.toml should be valid TOML");
        assert!(parsed.contains_key("bash"));
        assert!(parsed.contains_key("read"));
    }

    #[test]
    fn pascal_to_snake_cases() {
        assert_eq!(pascal_to_snake("Bash"), "bash");
        assert_eq!(pascal_to_snake("WebFetch"), "web_fetch");
        assert_eq!(pascal_to_snake("CodeExecution"), "code_execution");
        assert_eq!(pascal_to_snake("Read"), "read");
        assert_eq!(pascal_to_snake("already_snake"), "already_snake");
        // Consecutive uppercase should stay as a single word
        assert_eq!(pascal_to_snake("LS"), "ls");
        assert_eq!(pascal_to_snake("XMLParser"), "xml_parser");
        assert_eq!(pascal_to_snake("HTMLElement"), "html_element");
    }

    #[test]
    fn escape_toml_string_handles_special_chars() {
        assert_eq!(escape_toml_string("simple"), "simple");
        assert_eq!(escape_toml_string(r#"has "quotes""#), r#"has \"quotes\""#);
        assert_eq!(escape_toml_string(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn env_file_from_vars_quoted() {
        let dir = TempDir::new().unwrap();
        let vars = vec![
            ("ZZZ_VAR".to_string(), "last".to_string()),
            ("AAA_VAR".to_string(), "first".to_string()),
            ("TOKEN".to_string(), "base64value=".to_string()),
        ];
        let count = write_env_file_from_vars(&vars, dir.path()).unwrap();
        assert_eq!(count, 3);

        let content = fs::read_to_string(dir.path().join(".env")).unwrap();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines[0], r#"ZZZ_VAR="last""#);
        assert_eq!(lines[1], r#"AAA_VAR="first""#);
        assert_eq!(lines[2], r#"TOKEN="base64value=""#);
    }

    #[test]
    fn env_file_empty_vars() {
        let dir = TempDir::new().unwrap();
        let vars: Vec<(String, String)> = vec![];
        let count = write_env_file_from_vars(&vars, dir.path()).unwrap();
        assert_eq!(count, 0);
        assert!(!dir.path().join(".env").exists());
    }

    #[test]
    fn env_file_parseable_by_dotenvy() {
        let dir = TempDir::new().unwrap();
        let base64_val = "ABSKQmVkcm9ja0FQSUtleS1maHJpLWF0LTM5OTY4MzMz\
                          NzUwMjpNZ20rS3luMjQ4anU0QzBWSDlWRXV0bGJHZkM1\
                          dTZVOEwxR2lRenRsZEU5T1Y0TUhXNUUxTVA3Q3d3bz0=";
        let vars = vec![
            ("AWS_BEARER_TOKEN_BEDROCK".to_string(), base64_val.to_string()),
            ("ANTHROPIC_API_KEY".to_string(), "sk-ant-123".to_string()),
        ];
        write_env_file_from_vars(&vars, dir.path()).unwrap();

        // Verify dotenvy can parse the generated .env and recover exact values
        let parsed: HashMap<String, String> =
            dotenvy::from_path_iter(dir.path().join(".env"))
                .expect("dotenvy should open .env")
                .filter_map(|r| r.ok())
                .collect();

        assert_eq!(parsed.get("AWS_BEARER_TOKEN_BEDROCK").unwrap(), base64_val);
        assert_eq!(parsed.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-123");
    }

    /// End-to-end test using a realistic Claude settings.json (matching the
    /// user's actual config structure) to verify parse → prompt → write flow.
    #[test]
    fn wizard_realistic_claude_settings() {
        let src_dir = TempDir::new().unwrap();
        let dest_dir = TempDir::new().unwrap();

        let settings = serde_json::json!({
            "$schema": "https://json.schemastore.org/claude-code-settings.json",
            "model": "us.anthropic.claude-opus-4-6-v1",
            "permissions": {
                "defaultMode": "bypassPermissions",
                "allow": [
                    "Bash(cargo *)",
                    "Bash(git *)",
                    "Read(**)",
                    "Edit(**)",
                    "Write(**)",
                    "Glob(**)",
                    "Grep(**)",
                    "LS(**)",
                    "WebFetch(domain:docs.anthropic.com)",
                    "TodoRead",
                    "WebSearch"
                ],
                "deny": [
                    "Bash(rm -rf *)",
                    "Bash(sudo *)",
                    "Edit(~/.bashrc)",
                    "Read(~/.ssh/**)"
                ]
            },
            "env": {
                "CLAUDE_CODE_USE_BEDROCK": "true",
                "DISABLE_PROMPT_CACHING": "true",
                "DISABLE_TELEMETRY": "true",
                "DISABLE_ERROR_REPORTING": "true",
                "DISABLE_BUG_COMMAND": "true",
                "CLAUDE_CODE_ENABLE_TELEMETRY": "false",
                "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS": "true",
                "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "1",
                "AWS_BEARER_TOKEN_BEDROCK": "ABSKbase64token=",
                "_ANTHROPIC_MODEL": "us.anthropic.claude-sonnet-4-5-v1:0",
                "__ANTHROPIC_MODEL": "us.anthropic.claude-opus-4-7-v1:0"
            }
        });
        let settings_path = src_dir.path().join("settings.json");
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let config = parse_claude_config(&settings_path).unwrap();

        // Only AWS_BEARER_TOKEN_BEDROCK should pass the filter
        assert_eq!(config.env_vars.len(), 1);
        assert_eq!(config.env_vars[0].0, "AWS_BEARER_TOKEN_BEDROCK");

        // "TodoRead" and "WebSearch" have no parens → filtered out by parser
        // "LS(**)" → should be "ls" not "l_s"
        let ls_rules: Vec<_> = config
            .allow_rules
            .iter()
            .filter(|(tool, _)| tool == "ls")
            .collect();
        assert_eq!(ls_rules.len(), 1, "LS(**) should map to tool name 'ls'");
        assert_eq!(ls_rules[0].1, "**");

        // Accept all categories
        let input = b"\n\n\n\n";
        let mut reader = Cursor::new(&input[..]);
        let mut output = Vec::new();

        let result =
            run_wizard_inner(&config, dest_dir.path(), &mut reader, &mut output).unwrap();
        assert_eq!(
            result.as_deref(),
            Some("bedrock/us.anthropic.claude-opus-4-6-v1")
        );

        // Verify config.toml
        let config_content =
            fs::read_to_string(dest_dir.path().join("init.lua")).unwrap();
        assert!(config_content.contains("always_yolo = true"));
        assert!(config_content.contains("bedrock/us.anthropic.claude-opus-4-6-v1"));

        // Verify .env is parseable by dotenvy and has correct value
        let parsed: HashMap<String, String> =
            dotenvy::from_path_iter(dest_dir.path().join(".env"))
                .expect("dotenvy should open .env")
                .filter_map(|r| r.ok())
                .collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed.get("AWS_BEARER_TOKEN_BEDROCK").unwrap(),
            "ABSKbase64token="
        );

        // Verify permissions.toml is valid TOML with correct tool names
        let perms_content =
            fs::read_to_string(dest_dir.path().join("permissions.toml")).unwrap();
        let parsed_toml: toml::Table = perms_content
            .parse()
            .expect("permissions.toml should be valid TOML");

        assert!(parsed_toml.contains_key("bash"));
        assert!(parsed_toml.contains_key("read"));
        assert!(parsed_toml.contains_key("edit"));
        assert!(parsed_toml.contains_key("write"));
        assert!(parsed_toml.contains_key("glob"));
        assert!(parsed_toml.contains_key("grep"));
        assert!(parsed_toml.contains_key("ls"), "LS should map to 'ls', not 'l_s'");
        assert!(!parsed_toml.contains_key("l_s"), "pascal_to_snake should not split LS into l_s");
        assert!(parsed_toml.contains_key("web_fetch"));
    }
}
