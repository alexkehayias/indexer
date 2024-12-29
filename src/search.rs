use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use itertools::Itertools;
use rusqlite::{Connection, Result};
use serde::Serialize;
use serde_json::json;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy};
use zerocopy::AsBytes;

use super::schema::note_schema;

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

fn fulltext_search(index_path: &str, query: &str, limit: usize) -> Vec<SearchHit> {
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
    let idx = Index::open(index_path).expect("Unable to open index");
    let title = schema.get_field("title").unwrap();
    let body = schema.get_field("body").unwrap();

    let reader = idx
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .expect("Reader failed to load");

    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(&idx, vec![title, body]);
    let query = query_parser
        .parse_query(query)
        .expect("Failed to parse query");

    searcher
        .search(&query, &TopDocs::with_limit(limit))
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
        .collect()
}

/// Returns the note ID and similarity distance for the query. Results
/// are ordered by ascending distance because sqlite-vec only supports
/// ascending distance.
pub fn search_similar_notes(db: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();
    let query_vector = embeddings_model.embed(vec![query], None).unwrap();
    let q = query_vector[0].clone();
    let result: Vec<SearchHit> = db
        .prepare(
            r"
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
        ",
        )?
        .query_map([q.as_bytes(), limit.as_bytes(), limit.as_bytes()], |r| {
            Ok(SearchHit {
                r#type: SearchHitType::Similarity,
                id: r.get(0)?,
                score: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<SearchHit>, _>>()?;
    Ok(result)
}

#[derive(Serialize)]
pub struct SearchResult {
    id: String,
    r#type: String,
    title: String,
    file_name: String,
    tags: Option<String>,
    is_task: bool,
    task_status: Option<String>,
    body: String,
}

// Performs a full-text search of all notes for the given query. If
// `include_similarity`, also includes vector search results appended
// to the end of the list of results. This way, if there is a keyword
// search miss, there may be semantically similar results.
pub fn search_notes(
    index_path: &str,
    db: &Connection,
    include_similarity: bool,
    query: &str,
    limit: usize,
) -> Vec<SearchResult> {
    let search_hits = if include_similarity {
        let mut result = fulltext_search(index_path, query, limit);
        let similarity_query = query.replace("-title:journal ", "");
        let mut vec_search_result =
            search_similar_notes(db, &similarity_query, limit).unwrap_or_default();

        // Combine the results, dedupe, then sort by score
        result.append(&mut vec_search_result);
        result.into_iter().unique_by(|i| i.id.clone()).collect()
    } else {
        fulltext_search(index_path, query, limit)
    };

    // Search the db for the metadata and construct results
    let result_ids: Vec<String> = search_hits.iter().map(|i| i.id.clone()).collect();
    let result_ids_serialized = json!(result_ids);
    let result_ids_str = result_ids_serialized.to_string();

    let results: Vec<SearchResult> = db
        .prepare(
            r"
          SELECT
            id,
            type,
            file_name,
            title,
            tags,
            body,
            status
          FROM note_meta
          WHERE note_meta.id in (SELECT value from json_each(?))
        ",
        )
        .unwrap()
        .query_map([result_ids_str.as_bytes()], |r| {
            let maybe_task_status: Option<String> = r.get(6)?;
            Ok(SearchResult {
                id: r.get(0)?,
                r#type: r.get(1)?,
                file_name: r.get(2)?,
                title: r.get(3)?,
                tags: r.get(4)?,
                body: r.get(5)?,
                is_task: maybe_task_status.is_some(),
                task_status: maybe_task_status,
            })
        })
        .unwrap()
        .collect::<Result<Vec<SearchResult>, _>>()
        .unwrap();

    results
}
