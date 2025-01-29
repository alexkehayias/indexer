use std::fmt;

use handlebars::Handlebars;

#[derive(Debug)]
pub enum Prompt {
    NoteSummary,
}

impl fmt::Display for Prompt {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

// Implement the Into trait so that Prompt can be converted to an &str
impl From<Prompt> for String {
    fn from(item: Prompt) -> String {
        format!("{:?}", item)
    }
}

const NOTES_PROMPT: &str = r"
Summarize this context (CONTEXT) concisely. Always include a list of sources (SOURCES) with the title and file name if available.

CONTEXT:
{{context}}
";

pub fn templates<'a>() -> Handlebars<'a> {
    let mut registry = Handlebars::new();
    registry.set_strict_mode(true);
    registry
        .register_template_string(&Prompt::NoteSummary.to_string(), NOTES_PROMPT)
        .expect("Failed to register template");
    registry
}
