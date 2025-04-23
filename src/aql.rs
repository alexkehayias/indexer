use winnow::ascii::{alphanumeric1, space0};
use winnow::combinator::*;
use winnow::error::{ErrMode, InputError};
use winnow::prelude::*;
use winnow::token::{literal, take_while};

#[derive(Debug, PartialEq)]
pub enum RangeOp {
    Lt,
    Lte,
    Gt,
    Gte,
}

#[derive(Debug, PartialEq)]
pub enum Expr {
    Term {
        field: Option<String>,
        value: String,
        phrase: bool,
        negated: bool,
    },
    Range {
        field: String,
        op: RangeOp,
        value: String,
        negated: bool,
    },
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

pub fn parse_query(input: &str) -> Result<Expr, ErrMode<InputError<&str>>> {
    let mut input = input;
    parse_expr(&mut input)
}

fn parse_expr<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    parse_or(input)
}

fn parse_or<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let mut lhs = parse_and(input)?;
    while preceded(space0, tag_no_case("OR"))
        .parse_next(input)
        .is_ok()
    {
        let rhs = parse_and(input)?;
        lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
    }
    Ok(lhs)
}

fn parse_and<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let mut lhs = parse_not(input)?;

    loop {
        let checkpoint = *input;

        if preceded(space0, tag_no_case("AND"))
            .parse_next(input)
            .is_ok()
        {
            *input = checkpoint;
            break;
        }

        if let Ok(rhs) = parse_not(input) {
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        } else {
            break;
        }
    }

    Ok(lhs)
}

fn parse_not<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let negated = opt(alt((literal("-"), tag_no_case("NOT"))))
        .parse_next(input)?
        .is_some();
    let mut expr = parse_term(input)?;
    match &mut expr {
        Expr::Term { negated: n, .. } => *n = *n || negated,
        Expr::Range { negated: n, .. } => *n = *n || negated,
        _ => (),
    }

    Ok(expr)
}

fn parse_term<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    alt((parse_range_expr, parse_fielded_term, parse_default_term)).parse_next(input)
}

fn parse_range_expr<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let negated = opt(literal("-")).parse_next(input)?.is_some();
    let field: &str = alphanumeric1.parse_next(input)?;
    literal(":").parse_next(input)?;
    let op = alt((
        literal(">=").map(|_| RangeOp::Gte),
        literal("<=").map(|_| RangeOp::Lte),
        literal(">").map(|_| RangeOp::Gt),
        literal("<").map(|_| RangeOp::Lt),
    ))
    .parse_next(input)?;
    let value = take_while(1.., |c: char| !c.is_whitespace() && c != ')').parse_next(input)?;
    Ok(Expr::Range {
        field: field.to_string(),
        op,
        value: value.to_string(),
        negated,
    })
}

fn parse_fielded_term<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let negated = opt(literal("-")).parse_next(input)?.is_some();
    let field: &str = alphanumeric1.parse_next(input)?;
    literal(":").parse_next(input)?;

    let term_parser = alt((
        delimited(literal("\""), take_while(1.., |c| c != '"'), literal("\""))
            .map(|s: &str| (s.to_string(), true)),
        take_while(1.., |c: char| !c.is_whitespace() && c != ')' && c != ',')
            .map(|s: &str| (s.to_string(), false)),
    ));

    let values: Vec<(String, bool)> =
        separated(1.., term_parser, literal(",")).parse_next(input)?;

    if values.len() == 1 {
        Ok(Expr::Term {
            field: Some(field.to_string()),
            value: values[0].0.clone(),
            phrase: values[0].1,
            negated,
        })
    } else {
        let mut terms = values.into_iter().map(|(value, phrase)| Expr::Term {
            field: Some(field.to_string()),
            value,
            phrase,
            negated,
        });
        let first = terms.next().unwrap();
        Ok(terms.fold(first, |acc, term| Expr::And(Box::new(acc), Box::new(term))))
    }
}

fn parse_default_term<'a>(input: &mut &'a str) -> Result<Expr, ErrMode<InputError<&'a str>>> {
    let value = alt((
        delimited(literal("\""), take_while(1.., |c| c != '"'), literal("\""))
            .map(|s: &str| (s.to_string(), true)),
        take_while(1.., |c: char| !c.is_whitespace() && c != ')')
            .map(|s: &str| (s.to_string(), false)),
    ))
    .parse_next(input)?;
    Ok(Expr::Term {
        field: None,
        value: value.0,
        phrase: value.1,
        negated: false,
    })
}

fn tag_no_case<'a>(
    tag_str: &'static str,
) -> impl Parser<&'a str, &'a str, ErrMode<InputError<&'a str>>> {
    move |input: &mut &'a str| {
        let len = tag_str.len();
        let (head, tail) = input.split_at(len.min(input.len()));
        if head.eq_ignore_ascii_case(tag_str) {
            *input = tail;
            Ok(head)
        } else {
            Err(ErrMode::Backtrack(InputError::at(*input)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_range() {
        let result = parse_query("date:>2024-01-01").unwrap();
        assert_eq!(
            result,
            Expr::Range {
                field: "date".into(),
                op: RangeOp::Gt,
                value: "2024-01-01".into(),
                negated: false,
            }
        );
    }

    #[test]
    fn test_negated_range() {
        let result = parse_query("-price:<=100").unwrap();
        assert_eq!(
            result,
            Expr::Range {
                field: "price".into(),
                op: RangeOp::Lte,
                value: "100".into(),
                negated: true,
            }
        );
    }

    #[test]
    fn test_multiple_terms() {
        let result = parse_query("title:testing tags:meeting date:>2025-01-01").unwrap();
        assert_eq!(
            result,
            Expr::And(
                Box::new(Expr::And(
                    Box::new(Expr::Term {
                        field: Some(String::from("title")),
                        value: String::from("testing"),
                        phrase: false,
                        negated: false
                    }),
                    Box::new(Expr::Term {
                        field: Some(String::from("tags")),
                        value: String::from("meeting"),
                        phrase: false,
                        negated: false
                    })
                )),
                Box::new(Expr::Range {
                    field: String::from("date"),
                    op: RangeOp::Gt,
                    value: String::from("2025-01-01"),
                    negated: false
                })
            )
        );
    }

    #[test]
    fn test_comma_separated_terms() {
        let result = parse_query("tags:work,urgent").unwrap();
        assert_eq!(
            result,
            Expr::And(
                Box::new(Expr::Term {
                    field: Some(String::from("tags")),
                    value: String::from("work"),
                    phrase: false,
                    negated: false
                }),
                Box::new(Expr::Term {
                    field: Some(String::from("tags")),
                    value: String::from("urgent"),
                    phrase: false,
                    negated: false
                })
            ),
        );
    }
}
