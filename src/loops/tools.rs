//! Tool-set resolution for loops.
//!
//! The loop system prompt ([`super::prompt`]) tells the model to keep its
//! continuity in files under `state/`, which requires the bash tool. Every loop
//! therefore always includes bash, even when a caller supplies a custom tool
//! list.

/// Tools a loop uses when none are configured.
pub const DEFAULT_LOOP_TOOLS: [&str; 3] = ["bash", "python", "notify"];

/// Tools a loop always has, regardless of its configured list. `bash` backs the
/// `state/` file protocol in the loop system prompt.
pub const REQUIRED_LOOP_TOOLS: [&str; 1] = ["bash"];

/// Resolve a loop's tool names: the configured list with the required tools
/// ensured present (appended in order if missing), or the defaults when nothing
/// is configured.
pub fn resolve_loop_tools(configured: &[String]) -> Vec<String> {
    let mut tools: Vec<String> = if configured.is_empty() {
        DEFAULT_LOOP_TOOLS.iter().map(|s| s.to_string()).collect()
    } else {
        configured.to_vec()
    };
    for required in REQUIRED_LOOP_TOOLS {
        if !tools.iter().any(|name| name == required) {
            tools.push(required.to_string());
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn empty_config_uses_defaults() {
        assert_eq!(
            resolve_loop_tools(&[]),
            names(&["bash", "python", "notify"])
        );
    }

    #[test]
    fn bash_is_appended_when_missing() {
        assert_eq!(
            resolve_loop_tools(&names(&["notify"])),
            names(&["notify", "bash"])
        );
    }

    #[test]
    fn configured_list_is_preserved_when_bash_present() {
        assert_eq!(
            resolve_loop_tools(&names(&["bash", "calendar"])),
            names(&["bash", "calendar"])
        );
    }
}
