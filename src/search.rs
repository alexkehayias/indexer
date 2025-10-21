use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use itertools::Itertools;
use tokio_rusqlite::{Connection, Result};
use serde::Serialize;
use serde_json::json;
use tantivy::collector::TopDocs;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy};
use zerocopy::IntoBytes;

use crate::aql::{self};
use crate::fts::schema::note_schema;
use crate::query::{aql_to_index_query, expr_to_sql, query_to_similarity};

#[derive(Serialize)]
pub enum SearchHitType {
    #[serde(rename = "full_text")]
    FullText,
    #[serde(rename = "similarity")]
    Similarity,
}

#[derive(Serialize)]
pub struct SearchHit {
    pub id: String,
    pub r#type: SearchHitType,
    pub score: f32,
}

fn fulltext_search(index_path: &str, query: &aql::Expr, limit: usize) -> Result<Vec<SearchHit>> {
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
    let idx = Index::open(index_path).expect("Unable to open index");

    let reader = idx
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .expect("Reader failed to load");

    let searcher = reader.searcher();

    // Parse query using custom parser
    let index_query = aql_to_index_query(query, &schema);

    if let Some(idx_query) = index_query {
        let results = searcher
            .search(&idx_query, &TopDocs::with_limit(limit))
            .expect("Search failed")
            .iter()
            .map(|(score, doc_addr)| {
                let doc = searcher
                    .doc::<TantivyDocument>(*doc_addr)
                    .expect("Doc not found")
                    .to_named_doc(&schema)
                    .0;

                let id_val = doc.get("id").unwrap()[0]
                    .as_ref()
                    .as_str()
                    .unwrap()
                    .to_string();

                SearchHit {
                    id: id_val,
                    r#type: SearchHitType::FullText,
                    score: *score,
                }
            })
            .collect();
        Ok(results)
    } else {
        // This can happen if there are no searchable fields in the
        // index like when the only fields used are handled by SQL
        Ok(Vec::new())
    }
}

/// Returns the note ID and similarity distance for the query. Results
/// are ordered by ascending distance because sqlite-vec only supports
/// ascending distance.
pub async fn search_similar_notes(
    db: &Connection,
    query: &aql::Expr,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    // Extract the relevant text to use for similar search from the
    // AQL query. It's possible there is nothing to use for a
    // similarity search. This can happen when the query is entirely
    // fields that are not valid for similarity like a status field or
    // a date field.
    let similarity_string = query_to_similarity(query);
    if similarity_string.is_none() {
        return Ok(Vec::new());
    }

    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();
    let query_vector = embeddings_model
        .embed(vec![similarity_string.unwrap()], None)
        .unwrap();
    let q = query_vector[0].clone();
    let result: Vec<SearchHit> = db.call(move |conn| {
        let mut stmt = conn.prepare(
            r#"
          SELECT
            note_meta.id,
            note_meta.file_name,
            note_meta.title,
            note_meta.tags,
            note_meta.body,
            distance
          FROM vec_items
          JOIN note_meta on note_meta_id=note_meta.id
          AND LOWER(note_meta.title) NOT LIKE LOWER('%journal%')
          WHERE embedding MATCH ? AND k = ?
          ORDER BY distance
          LIMIT ?
        "#,
        )?;
        let found = stmt.query_map([q.as_bytes(), limit.as_bytes(), limit.as_bytes()], |r| {
            Ok(SearchHit {
                r#type: SearchHitType::Similarity,
                id: r.get(0)?,
                score: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<SearchHit>, _>>()?;
        Ok(found)
    }).await?;
    Ok(result)
}

#[derive(Serialize)]
pub struct SearchResult {
    id: String,
    r#type: String,
    title: String,
    category: String,
    file_name: String,
    tags: Option<String>,
    is_task: bool,
    task_status: Option<String>,
    task_scheduled: Option<String>,
    task_deadline: Option<String>,
    task_closed: Option<String>,
    meeting_date: Option<String>,
    body: String,
}

// Performs a full-text search of all notes for the given query. If
// `include_similarity`, also includes vector search results appended
// to the end of the list of results. This way, if there is a keyword
// search miss, there may be semantically similar results.
pub async fn search_notes(
    index_path: &str,
    db: &Connection,
    include_similarity: bool,
    query: &aql::Expr,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    // The limit of search hits needs to be high enough here for broad
    // queries like `status:todo deadline:>2025-04-01` otherwise
    // results will be unexpectedly missing
    // TODO: This approach doesn't work well with similarity search
    // because full text results will drown out the similarity search
    // unless we have a really good way of combining results by
    // relevance
    let mut search_hits = fulltext_search(index_path, query, 10000).unwrap_or_else(|_| Vec::new());
    if include_similarity {
        let mut vec_search_result = search_similar_notes(db, query, limit).await.unwrap_or_default();

        // Combine the results, dedupe, then sort by score
        search_hits.append(&mut vec_search_result);
        search_hits = search_hits
            .into_iter()
            .unique_by(|i| i.id.clone())
            .collect();
    }

    // Search the db for the metadata and construct results
    let result_ids: Vec<String> = search_hits.iter().map(|i| i.id.clone()).collect();
    let result_ids_serialized = json!(result_ids);
    let result_ids_str = result_ids_serialized.to_string();

    let mut where_clauses = Vec::new();

    if !result_ids.is_empty() {
        where_clauses.push("note_meta.id in (SELECT value from json_each(?))".to_string());
    }

    if let Some(extra_sql) = expr_to_sql(query) {
        where_clauses.push(extra_sql);
    }

    let where_clause = if !where_clauses.is_empty() {
        format!("WHERE {}", where_clauses.join(" AND "))
    } else {
        "".to_string()
    };

    let sql = format!(
        r#"
        SELECT
          id,
          type,
          category,
          file_name,
          title,
          tags,
          body,
          status,
          scheduled,
          deadline,
          closed,
          date
        FROM note_meta
        {}
        ORDER BY date DESC, deadline DESC, scheduled DESC, closed DESC
        LIMIT {}
    "#,
        where_clause, limit
    );

    let results: Vec<SearchResult> = if !result_ids.is_empty() {
        db.call(move |conn| {
            let mut stmt = conn.prepare(&sql).unwrap();
            let found = stmt.query_map([result_ids_str.as_bytes()], |r| {
                let maybe_task_status: Option<String> = r.get(7)?;
                Ok(SearchResult {
                    id: r.get(0)?,
                    r#type: r.get(1)?,
                    category: r.get(2)?,
                    file_name: r.get(3)?,
                    title: r.get(4)?,
                    tags: r.get(5)?,
                    body: r.get(6)?,
                    is_task: maybe_task_status.is_some(),
                    task_status: maybe_task_status,
                    task_scheduled: r.get(8)?,
                    task_deadline: r.get(9)?,
                    task_closed: r.get(10)?,
                    meeting_date: r.get(11)?,
                })
            })?
            .collect::<std::result::Result<Vec<SearchResult>, _>>()?;
            Ok(found)
        }).await.unwrap()
    } else {
        db.call(move |conn| {
            let mut stmt = conn.prepare(&sql).unwrap();
            let found = stmt.query_map([], |r| {
                let maybe_task_status: Option<String> = r.get(7)?;
                Ok(SearchResult {
                    id: r.get(0)?,
                    r#type: r.get(1)?,
                    category: r.get(2)?,
                    file_name: r.get(3)?,
                    title: r.get(4)?,
                    tags: r.get(5)?,
                    body: r.get(6)?,
                    is_task: maybe_task_status.is_some(),
                    task_status: maybe_task_status,
                    task_scheduled: r.get(8)?,
                    task_deadline: r.get(9)?,
                    task_closed: r.get(10)?,
                    meeting_date: r.get(11)?,
                })
            })?
            .collect::<std::result::Result<Vec<SearchResult>, _>>()?;
            Ok(found)
        }).await.unwrap()
    };
    Ok(results)
}
