//! Diagnostic report for `/doctor` — gathers local environment context
//! that the agent can analyze to detect configuration and connectivity issues.

use std::fmt::Write;
use std::path::Path;
use std::{env, fs};

use crate::global_dir;

const CONFIG_FILE: &str = "config.toml";
const PERMISSIONS_FILE: &str = "permissions.toml";
const ENV_FILE: &str = ".env";

/// Build a human-readable diagnostic report string.
pub fn gather_report(current_model: &str) -> String {
    let mut out = String::with_capacity(2048);

    writeln!(out, "## Current model\n").unwrap();
    writeln!(out, "{current_model}\n").unwrap();

    section_config_files(&mut out);
    section_env_vars(&mut out);
    section_provider_auth(&mut out, current_model);
    section_tools(&mut out);

    out
}

fn section_config_files(out: &mut String) {
    writeln!(out, "## Config files\n").unwrap();

    let global = global_dir();
    match &global {
        Some(dir) => writeln!(out, "Config directory: {}", dir.display()).unwrap(),
        None => {
            writeln!(out, "WARNING: cannot determine config directory").unwrap();
            return;
        }
    }
    let dir = global.unwrap();

    check_file(out, &dir.join(CONFIG_FILE));
    check_file(out, &dir.join(ENV_FILE));
    check_file(out, &dir.join(PERMISSIONS_FILE));
    writeln!(out).unwrap();
}

fn check_file(out: &mut String, path: &Path) {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    if path.is_file() {
        match fs::metadata(path) {
            Ok(m) => writeln!(out, "  {name}: exists ({} bytes)", m.len()).unwrap(),
            Err(e) => writeln!(out, "  {name}: exists but unreadable ({e})").unwrap(),
        }
    } else {
        writeln!(out, "  {name}: missing").unwrap();
    }
}

fn section_env_vars(out: &mut String) {
    writeln!(out, "## Environment variables\n").unwrap();

    let vars = [
        // Anthropic
        "ANTHROPIC_API_KEY",
        // AWS / Bedrock
        "AWS_BEARER_TOKEN_BEDROCK",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_REGION",
        "AWS_DEFAULT_REGION",
        "AWS_BEDROCK_REGION",
        "AWS_BEDROCK_CROSS_REGION",
        "AWS_PROFILE",
        // OpenAI
        "OPENAI_API_KEY",
        // Google
        "GEMINI_API_KEY",
    ];

    for name in vars {
        let status = match env::var(name) {
            Ok(v) if v.is_empty() => "set but EMPTY".to_string(),
            Ok(v) => format!("set ({} chars)", v.len()),
            Err(_) => "not set".to_string(),
        };
        writeln!(out, "  {name}: {status}").unwrap();
    }
    writeln!(out).unwrap();
}

fn section_provider_auth(out: &mut String, model_spec: &str) {
    writeln!(out, "## Provider authentication\n").unwrap();

    let provider = model_spec.split_once('/').map(|(p, _)| p).unwrap_or("unknown");
    writeln!(out, "Active provider: {provider}").unwrap();

    match provider {
        "bedrock" => {
            let has_bearer = env::var("AWS_BEARER_TOKEN_BEDROCK")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            let has_keys = env::var("AWS_ACCESS_KEY_ID").is_ok()
                && env::var("AWS_SECRET_ACCESS_KEY").is_ok();
            let has_profile = dirs::home_dir()
                .map(|h| h.join(".aws").join("credentials").is_file())
                .unwrap_or(false);

            if has_bearer {
                writeln!(out, "  Auth method: bearer token (AWS_BEARER_TOKEN_BEDROCK)").unwrap();
            } else if has_keys {
                writeln!(out, "  Auth method: access key (AWS_ACCESS_KEY_ID)").unwrap();
            } else if has_profile {
                writeln!(out, "  Auth method: shared credentials (~/.aws/credentials)").unwrap();
            } else {
                writeln!(out, "  WARNING: no Bedrock credentials found").unwrap();
            }

            let region = env::var("AWS_BEDROCK_REGION")
                .or_else(|_| env::var("AWS_DEFAULT_REGION"))
                .or_else(|_| env::var("AWS_REGION"))
                .unwrap_or_else(|_| "us-east-1 (default)".to_string());
            writeln!(out, "  Region: {region}").unwrap();
        }
        "anthropic" => {
            let has_key = env::var("ANTHROPIC_API_KEY")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if has_key {
                writeln!(out, "  Auth: ANTHROPIC_API_KEY is set").unwrap();
            } else {
                writeln!(out, "  WARNING: ANTHROPIC_API_KEY not set").unwrap();
            }
        }
        "openai" => {
            let has_key = env::var("OPENAI_API_KEY")
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if has_key {
                writeln!(out, "  Auth: OPENAI_API_KEY is set").unwrap();
            } else {
                writeln!(out, "  WARNING: OPENAI_API_KEY not set").unwrap();
            }
        }
        _ => {
            writeln!(out, "  (no specific auth checks for this provider)").unwrap();
        }
    }
    writeln!(out).unwrap();
}

fn section_tools(out: &mut String) {
    writeln!(out, "## System tools\n").unwrap();

    let tools = ["git", "curl", "jq"];
    for tool in tools {
        let found = which(tool);
        if found {
            writeln!(out, "  {tool}: found").unwrap();
        } else {
            writeln!(out, "  {tool}: NOT found").unwrap();
        }
    }
    writeln!(out).unwrap();
}

fn which(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gather_report_contains_sections() {
        let report = gather_report("bedrock/us.anthropic.claude-opus-4-6-v1:0");
        assert!(report.contains("## Current model"));
        assert!(report.contains("## Config files"));
        assert!(report.contains("## Environment variables"));
        assert!(report.contains("## Provider authentication"));
        assert!(report.contains("## System tools"));
        assert!(report.contains("bedrock/us.anthropic.claude-opus-4-6-v1:0"));
    }

    #[test]
    fn gather_report_detects_provider() {
        let report = gather_report("anthropic/claude-sonnet-4-5-20250514");
        assert!(report.contains("Active provider: anthropic"));

        let report = gather_report("bedrock/us.anthropic.claude-opus-4-6-v1:0");
        assert!(report.contains("Active provider: bedrock"));
    }

    #[test]
    fn gather_report_lists_env_vars() {
        let report = gather_report("anthropic/claude-sonnet-4-5-20250514");
        assert!(report.contains("ANTHROPIC_API_KEY:"));
        assert!(report.contains("AWS_BEARER_TOKEN_BEDROCK:"));
        assert!(report.contains("OPENAI_API_KEY:"));
    }

    #[test]
    fn gather_report_checks_git() {
        let report = gather_report("anthropic/test");
        assert!(report.contains("git:"));
    }
}
