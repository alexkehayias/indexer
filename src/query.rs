use crate::aql::{Expr, RangeOp};
use tantivy::query::{BooleanQuery, TermQuery, AllQuery};
use tantivy::schema::{Schema, IndexRecordOption, Field};
use tantivy::Term;
use tantivy::query::{Query, Occur};
use std::ops::Bound;

fn parse_date_to_timestamp(date_str: &str) -> u64 {
    let parts: Vec<u32> = date_str.split('-').map(|s| s.parse().unwrap()).collect();
    let (year, month, day) = (parts[0], parts[1], parts[2]);

    // Calculate days since UNIX_EPOCH
    let days =
        (year as i64 - 1970) * 365 +
        ((year as i64 - 1969) / 4) -
        ((year as i64 - 1901) / 100) +
        ((year as i64 - 1601) / 400) +
        match month {
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
        } as i64 +
        day as i64 - 1;

    (days * 24 * 60 * 60) as u64
}

pub fn aql_to_index_query(expr: &Expr, schema: &Schema) -> Box<dyn Query> {
    match expr {
        Expr::Term { field, value, phrase: _, negated } => {
            let field_names = field.clone().unwrap_or_else(|| "__default".into());
            let fields: Vec<Field> = if field_names == "__default" {
                vec![schema.get_field("title").unwrap(), schema.get_field("body").unwrap()]
            } else {
                vec![schema.get_field(&field_names).unwrap()]
            };
            let terms: Vec<Box<dyn Query>> = fields.iter().map(|&field| {
                let term = Term::from_field_text(field, value);
                if *negated {
                    Box::new(
                        BooleanQuery::new(vec![
                            (Occur::Must, Box::new(AllQuery)),
                            (Occur::MustNot, Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>)])
                    )
                } else {
                    Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>
                }
            }).collect();

            if terms.len() > 1 {
                Box::new(BooleanQuery::from(terms.into_iter().map(|q| (Occur::Should, q)).collect::<Vec<(Occur, Box<dyn Query>)>>()))
            } else {
                terms.into_iter().next().unwrap()
            }
        },
        Expr::Range { field, op, value, negated } => {
            let field_name = field;
            let value = parse_date_to_timestamp(value);
            let (lower_bound, upper_bound) = match op {
                RangeOp::Lt => (Bound::Unbounded, Bound::Excluded(value)),
                RangeOp::Lte => (Bound::Unbounded, Bound::Included(value)),
                RangeOp::Gt => (Bound::Excluded(value), Bound::Unbounded),
                RangeOp::Gte => (Bound::Included(value), Bound::Unbounded),
            };

            let range_query = tantivy::query::RangeQuery::new_u64_bounds(
                field_name.to_string(), lower_bound, upper_bound
            );

            if *negated {
                Box::new(BooleanQuery::from(vec![(Occur::MustNot, Box::new(range_query) as Box<dyn Query>)]))
            } else {
                Box::new(range_query)
            }
        },
        Expr::And(left, right) => {
            let left_query = aql_to_index_query(left, schema);
            let right_query = aql_to_index_query(right, schema);
            Box::new(BooleanQuery::from(vec![(Occur::Must, left_query), (Occur::Must, right_query)]))
        },
        Expr::Or(left, right) => {
            let left_query = aql_to_index_query(left, schema);
            let right_query = aql_to_index_query(right, schema);
            Box::new(BooleanQuery::from(vec![(Occur::Should, left_query), (Occur::Should, right_query)]))
        },
        Expr::Group(exprs) => {
            let queries: Vec<(Occur, Box<dyn Query>)> = exprs.iter().map(|e| (Occur::Must, aql_to_index_query(e, schema))).collect();
            Box::new(BooleanQuery::from(queries))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aql::parse_query;
    use crate::schema::note_schema;

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
        assert!(matches!(query.as_any().downcast_ref::<BooleanQuery>(), Some(_)));
    }
}
