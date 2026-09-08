use std::cmp::min;
use std::fmt::Write as _;

use orgize::export::{Container, Event, TraversalContext, Traverser};
use orgize::{SyntaxElement, SyntaxNode};

#[derive(Default)]
pub struct MarkdownExport {
    output: String,
    inside_blockquote: bool,
    inside_list: bool,
}

impl MarkdownExport {
    /// Render syntax node to markdown string
    pub fn render(&mut self, node: &SyntaxNode) {
        let mut ctx = TraversalContext::default();
        self.element(SyntaxElement::Node(node.clone()), &mut ctx);
    }

    pub fn finish(self) -> String {
        self.output
    }

    fn follows_newline(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with(['\n', '\r']) {
            self.output += "\n";
        }
    }
}

impl Traverser for MarkdownExport {
    fn event(&mut self, event: Event, ctx: &mut TraversalContext) {
        match event {
            Event::Enter(Container::Drawer(_)) => {}
            Event::Leave(Container::Drawer(_)) => {}
            Event::Enter(Container::PropertyDrawer(_)) => {}
            Event::Leave(Container::PropertyDrawer(_)) => {}

            Event::Enter(Container::Document(_)) => {}
            Event::Leave(Container::Document(_)) => {}

            Event::Enter(Container::Headline(headline)) => {
                self.follows_newline();
                let level = min(headline.level(), 6);
                let _ = write!(&mut self.output, "{} ", "#".repeat(level));
                for elem in headline.title() {
                    self.element(elem, ctx);
                }
            }
            Event::Leave(Container::Headline(_)) => {}

            Event::Enter(Container::Paragraph(_)) => {}
            Event::Leave(Container::Paragraph(_)) => {
                if !self.inside_list {
                    self.output += "\n"
                }
            }

            Event::Enter(Container::Section(_)) => self.follows_newline(),
            Event::Leave(Container::Section(_)) => {}

            Event::Enter(Container::Italic(_)) => self.output += "*",
            Event::Leave(Container::Italic(_)) => self.output += "*",

            Event::Enter(Container::Bold(_)) => self.output += "**",
            Event::Leave(Container::Bold(_)) => self.output += "**",

            Event::Enter(Container::Strike(_)) => self.output += "~~",
            Event::Leave(Container::Strike(_)) => self.output += "~~",

            Event::Enter(Container::Underline(_)) => {}
            Event::Leave(Container::Underline(_)) => {}

            Event::Enter(Container::Verbatim(_))
            | Event::Leave(Container::Verbatim(_))
            | Event::Enter(Container::Code(_))
            | Event::Leave(Container::Code(_)) => self.output += "`",

            Event::Enter(Container::SourceBlock(block)) => {
                self.follows_newline();
                self.output += "```";
                if let Some(language) = block.language() {
                    self.output += &language;
                }
            }
            Event::Leave(Container::SourceBlock(_)) => self.output += "```\n",

            Event::Enter(Container::QuoteBlock(_)) => {
                self.inside_blockquote = true;
                self.follows_newline();
                self.output += "> ";
            }
            Event::Leave(Container::QuoteBlock(_)) => self.inside_blockquote = false,

            Event::Enter(Container::CommentBlock(_)) => self.output += "<!--",
            Event::Leave(Container::CommentBlock(_)) => self.output += "-->",

            Event::Enter(Container::Comment(_)) => self.output += "<!--",
            Event::Leave(Container::Comment(_)) => self.output += "-->",

            Event::Enter(Container::Subscript(_)) => self.output += "<sub>",
            Event::Leave(Container::Subscript(_)) => self.output += "</sub>",

            Event::Enter(Container::Superscript(_)) => self.output += "<sup>",
            Event::Leave(Container::Superscript(_)) => self.output += "</sup>",

            Event::Enter(Container::List(_list)) => {
                self.inside_list = true;
            }
            Event::Leave(Container::List(_list)) => {
                self.inside_list = false;
            }

            Event::Enter(Container::ListItem(list_item)) => {
                self.follows_newline();
                self.output += &" ".repeat(list_item.indent());
                self.output += &list_item.bullet();
            }
            Event::Leave(Container::ListItem(_)) => {}

            Event::Enter(Container::OrgTable(_table)) => {}
            Event::Leave(Container::OrgTable(_)) => {}
            Event::Enter(Container::OrgTableRow(_row)) => {}
            Event::Leave(Container::OrgTableRow(_row)) => {}
            Event::Enter(Container::OrgTableCell(_)) => {}
            Event::Leave(Container::OrgTableCell(_)) => {}

            Event::Enter(Container::Keyword(_)) => ctx.skip(),
            Event::Leave(Container::Keyword(_)) => {}

            Event::Enter(Container::Link(link)) => {
                let path = link.path();
                let path = path.trim_start_matches("file:");

                if link.is_image() {
                    let _ = write!(&mut self.output, "![]({path})");
                    return ctx.skip();
                }

                if !link.has_description() {
                    let _ = write!(&mut self.output, r#"[{}]({})"#, &path, &path);
                    return ctx.skip();
                }

                self.output += "[";
            }
            Event::Leave(Container::Link(link)) => {
                let _ = write!(&mut self.output, r#"]({})"#, &*link.path());
            }

            Event::Text(text) => {
                if self.inside_blockquote {
                    for (idx, line) in text.split('\n').enumerate() {
                        if idx != 0 {
                            self.output += "\n>  ";
                        }
                        self.output += line;
                    }
                } else {
                    self.output += &*text;
                }
            }

            Event::LineBreak(_) => {}

            Event::Snippet(_snippet) => {}

            Event::Rule(_) => self.output += "\n-----\n",

            Event::Timestamp(_timestamp) => {}

            Event::LatexFragment(_) => {}
            Event::LatexEnvironment(_) => {}

            Event::Entity(entity) => self.output += entity.utf8(),

            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MarkdownExport;
    use orgize::{Org, ast::Bold, rowan::ast::AstNode};

    #[test]
    fn test_it_works() {
        let org = Org::parse("* /hello/ *world*");
        let bold = org.first_node::<Bold>().unwrap();
        let mut markdown = MarkdownExport::default();
        markdown.render(bold.syntax());
        assert_eq!(markdown.finish(), "**world**");
    }
}
