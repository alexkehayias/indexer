use crate::aql::{Expr, RangeOp};
use std::ops::Bound;
use tantivy::Term;
use tantivy::query::{AllQuery, BooleanQuery, TermQuery};
use tantivy::query::{Occur, Query};
use tantivy::schema::{Field, IndexRecordOption, Schema};

fn parse_date_to_timestamp(date_str: &str) -> u64 {
    let parts: Vec<u32> = date_str.split('-').map(|s| s.parse().unwrap()).collect();
    let (year, month, day) = (parts[0], parts[1], parts[2]);

    // Calculate days since UNIX_EPOCH
    let days = (year as i64 - 1970) * 365 + ((year as i64 - 1969) / 4)
        - ((year as i64 - 1901) / 100)
        + ((year as i64 - 1601) / 400)
        + match month {
            1 => 0,
            2 => 31,
            3 => 59,
            4 => 90,
            5 => 120,
            6 => 151,
            7 => 181,
            8 => 212,
            9 => 243,
            10 => 273,
            11 => 304,
            12 => 334,
            _ => 0,
        } as i64
        + day as i64
        - 1;

    (days * 24 * 60 * 60) as u64
}

pub fn aql_to_index_query(expr: &Expr, schema: &Schema) -> Option<Box<dyn Query>> {
    fn is_sql_only_field(field: &str) -> bool {
        matches!(field, "scheduled" | "deadline" | "closed" | "date")
    }

    match expr {
        Expr::Term {
            field: Some(field), ..
        } if is_sql_only_field(field) => None,
        Expr::Range { field, .. } if is_sql_only_field(field) => None,
        Expr::Term {
            field,
            value,
            phrase: _,
            negated,
        } => {
            let field_names = field.clone().unwrap_or_else(|| "__default".into());
            let fields: Vec<Field> = if field_names == "__default" {
                vec![
                    schema.get_field("title").unwrap(),
                    schema.get_field("body").unwrap(),
                ]
            } else {
                vec![schema.get_field(&field_names).unwrap()]
            };
            let terms: Vec<Box<dyn Query>> = fields
                .iter()
                .map(|&field| {
                    let term = Term::from_field_text(field, value);
                    if *negated {
                        Box::new(BooleanQuery::new(vec![
                            (Occur::Must, Box::new(AllQuery)),
                            (
                                Occur::MustNot,
                                Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                                    as Box<dyn Query>,
                            ),
                        ]))
                    } else {
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>
                    }
                })
                .collect();

            if terms.len() > 1 {
                Some(Box::new(BooleanQuery::from(
                    terms
                        .into_iter()
                        .map(|q| (Occur::Should, q))
                        .collect::<Vec<(Occur, Box<dyn Query>)>>(),
                )))
            } else {
                Some(terms.into_iter().next().unwrap())
            }
        }
        Expr::Range {
            field,
            op,
            value,
            negated,
        } => {
            let field_name = field;
            let value = parse_date_to_timestamp(value);
            let (lower_bound, upper_bound) = match op {
                RangeOp::Lt => (Bound::Unbounded, Bound::Excluded(value)),
                RangeOp::Lte => (Bound::Unbounded, Bound::Included(value)),
                RangeOp::Gt => (Bound::Excluded(value), Bound::Unbounded),
                RangeOp::Gte => (Bound::Included(value), Bound::Unbounded),
            };

            let range_query = tantivy::query::RangeQuery::new_u64_bounds(
                field_name.to_string(),
                lower_bound,
                upper_bound,
            );

            if *negated {
                Some(Box::new(BooleanQuery::from(vec![(
                    Occur::MustNot,
                    Box::new(range_query) as Box<dyn Query>,
                )])))
            } else {
                Some(Box::new(range_query))
            }
        }
        Expr::And(left, right) => {
            // This handles the following cases:
            // - Left and right expressions have a query term
            // - Only the left expression has a query term
            // - Only the right expression has a query term
            // - Neither left or right expressions have a query term
            let left_query = aql_to_index_query(left, schema);
            let right_query = aql_to_index_query(right, schema);
            if let Some(lq) = left_query {
                if let Some(rq) = right_query {
                    Some(Box::new(BooleanQuery::from(vec![
                        (Occur::Must, lq),
                        (Occur::Must, rq),
                    ])))
                } else {
                    Some(Box::new(BooleanQuery::from(vec![(Occur::Must, lq)])))
                }
            } else if let Some(rq) = right_query {
                Some(Box::new(BooleanQuery::from(vec![(Occur::Must, rq)])))
            } else {
                None
            }
        }
        Expr::Or(left, right) => {
            let left_query = aql_to_index_query(left, schema);
            let right_query = aql_to_index_query(right, schema);
            if let Some(lq) = left_query {
                if let Some(rq) = right_query {
                    Some(Box::new(BooleanQuery::from(vec![
                        (Occur::Should, lq),
                        (Occur::Must, rq),
                    ])))
                } else {
                    Some(Box::new(BooleanQuery::from(vec![(Occur::Should, lq)])))
                }
            } else if let Some(rq) = right_query {
                Some(Box::new(BooleanQuery::from(vec![(Occur::Should, rq)])))
            } else {
                None
            }
        }
    }
}

pub fn expr_to_sql(expr: &Expr) -> Option<String> {
    fn is_allowed(field: &str) -> bool {
        matches!(field, "scheduled" | "deadline" | "closed" | "date")
    }

    match expr {
        Expr::Term {
            field: Some(field),
            value,
            negated,
            ..
        } if is_allowed(field) => {
            let cmp = if *negated { "!=" } else { "=" };
            Some(format!(
                r#"{} {} '{}'"#,
                field,
                cmp,
                value.replace('\'', "''")
            ))
        }
        Expr::Range {
            field,
            op,
            value,
            negated,
        } if is_allowed(field) => {
            let op_str = match op {
                RangeOp::Lt => {
                    if *negated {
                        ">="
                    } else {
                        "<"
                    }
                }
                RangeOp::Lte => {
                    if *negated {
                        ">"
                    } else {
                        "<="
                    }
                }
                RangeOp::Gt => {
                    if *negated {
                        "<="
                    } else {
                        ">"
                    }
                }
                RangeOp::Gte => {
                    if *negated {
                        "<"
                    } else {
                        ">="
                    }
                }
            };
            Some(format!(
                r#"{} {} '{}'"#,
                field,
                op_str,
                value.replace('\'', "''")
            ))
        }
        Expr::And(left, right) => {
            let l = expr_to_sql(left);
            let r = expr_to_sql(right);
            match (l, r) {
                (Some(l), Some(r)) => Some(format!("({} AND {})", l, r)),
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                _ => None,
            }
        }
        Expr::Or(left, right) => {
            let l = expr_to_sql(left);
            let r = expr_to_sql(right);
            match (l, r) {
                (Some(l), Some(r)) => Some(format!("({} OR {})", l, r)),
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn query_to_similarity(expr: &Expr) -> Option<String> {
    fn is_allowed(field: &str) -> bool {
        matches!(field, "title" | "body")
    }

    match expr {
        Expr::Term {
            field: Some(field),
            value,
            negated,
            ..
        } if is_allowed(field) => {
            if *negated {
                None
            } else {
                Some(value.to_owned())
            }
        }
        Expr::And(left, right) => {
            let l = query_to_similarity(left);
            let r = query_to_similarity(right);
            match (l, r) {
                (Some(l), Some(r)) => Some(format!("({} {})", l, r)),
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                _ => None,
            }
        }
        Expr::Or(left, right) => {
            let l = expr_to_sql(left);
            let r = expr_to_sql(right);
            match (l, r) {
                (Some(l), Some(r)) => Some(format!("({} {})", l, r)),
                (Some(l), None) => Some(l),
                (None, Some(r)) => Some(r),
                _ => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aql::parse_query;
    use crate::fts::schema::note_schema;

    #[test]
    fn test_aql_to_index_query() {
        // Define a simple schema
        let schema = note_schema();

        // Create an expression to test
        let expr_str = "title:testing tags:meeting date:>2025-01-01 I am testing";
        let expr = parse_query(expr_str).unwrap();

        // Convert expression to query
        let query = aql_to_index_query(&expr, &schema);

        // Assertions
        assert!(
            query
                .unwrap()
                .as_any()
                .downcast_ref::<BooleanQuery>()
                .is_some()
        );
    }

    #[test]
    fn test_expr_to_sql_term() {
        let expr = parse_query("scheduled:2025-04-20").unwrap();
        assert_eq!(
            expr_to_sql(&expr),
            Some("scheduled = '2025-04-20'".to_string())
        );

        let expr = parse_query("-closed:2024-01-01").unwrap();
        assert_eq!(
            expr_to_sql(&expr),
            Some("closed != '2024-01-01'".to_string())
        );
    }

    #[test]
    fn test_expr_to_sql_range() {
        let expr = parse_query("date:>2021-10-10").unwrap();
        assert_eq!(expr_to_sql(&expr), Some("date > '2021-10-10'".to_string()));

        let expr = parse_query("-deadline:<=2022-12-31").unwrap();
        assert_eq!(
            expr_to_sql(&expr),
            Some("deadline > '2022-12-31'".to_string())
        );
    }

    #[test]
    fn test_expr_to_sql_drops_unknown() {
        // 'priority' is not an allowed field; should yield None when it's alone.
        let expr = parse_query("priority:high").unwrap();
        assert_eq!(expr_to_sql(&expr), None);

        // If mixed with a valid field, only valid one appears in output.
        let expr = parse_query("priority:high scheduled:2024-12-12").unwrap();
        assert_eq!(
            expr_to_sql(&expr),
            Some("scheduled = '2024-12-12'".to_string())
        );
    }
}
