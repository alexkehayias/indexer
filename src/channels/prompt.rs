//! System-message section describing channels, loops, and the default timer
//! channels. Appended to the main chat's system prompt so the model knows how
//! to schedule work and which channels are available.

use std::time::Duration;

use super::default::DEFAULT_TIMER_CHANNELS;

/// Human-readable label for a timer interval (e.g. `every 30 minutes`).
fn interval_label(interval: Duration) -> String {
    let secs = interval.as_secs();
    if secs.is_multiple_of(3600) {
        let hours = secs / 3600;
        format!("every {hours} hour{}", if hours == 1 { "" } else { "s" })
    } else if secs.is_multiple_of(60) {
        format!("every {} minutes", secs / 60)
    } else {
        format!("every {secs} seconds")
    }
}

/// System-message section describing channels and loops, including the default
/// timer channels.
pub fn system_prompt_section() -> String {
    let channels = DEFAULT_TIMER_CHANNELS
        .iter()
        .map(|(id, interval)| format!("- {id}: {}", interval_label(*interval)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\n\n## Channels and Loops\n\
         Channels are streams of events. A loop is a persistent subscriber: each event \
         published to a channel runs an LLM turn with the loop's system prompt and tools. \
         Use the `create_loop` tool to create a loop subscribed to one or more channels, \
         `list_loops` to inspect them, and `delete_loop` to remove one. Loops keep running \
         across server restarts. Use `create_channel` to create a channel you own and \
         `emit_channel` to publish messages to it; only the agent that created a channel \
         can emit to it.\n\n\
         The following timer channels are always available; each publishes a `tick` event \
         with the current UTC time on a fixed interval:\n\
         {channels}\n\n\
         To run something on a schedule, create a loop subscribed to the matching timer channel."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_section_lists_default_channels() {
        let section = system_prompt_section();
        assert!(section.contains("## Channels and Loops"));
        assert!(section.contains("create_loop"));
        for (id, _) in DEFAULT_TIMER_CHANNELS {
            assert!(section.contains(id), "missing channel {id}");
        }
        assert!(section.contains("every 30 minutes"), "{section}");
        assert!(section.contains("every 1 hour"), "{section}");
        assert!(section.contains("every 4 hours"), "{section}");
        assert!(section.contains("every 12 hours"), "{section}");
    }
}
