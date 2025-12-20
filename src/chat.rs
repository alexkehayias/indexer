use anyhow::{Error, Result, anyhow, bail};
use futures_util::future::try_join_all;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_rusqlite::{Connection, params};

use crate::{
    openai::{
        BoxedToolCall, FunctionCall, FunctionCallFn, Message, Role, completion, completion_stream,
    },
    public::ChatSession,
};

async fn handle_tool_call(
    tools: &Vec<BoxedToolCall>,
    tool_call: &Value,
) -> Result<Vec<Message>, Error> {
    let tool_call_id = &tool_call["id"]
        .as_str()
        .ok_or(anyhow!("Tool call missing ID: {}", tool_call))?;
    let tool_call_function = &tool_call["function"];
    let tool_call_args = tool_call_function["arguments"]
        .as_str()
        .ok_or(anyhow!("Tool call missing arguments: {}", tool_call))?;
    let tool_call_name = tool_call_function["name"]
        .as_str()
        .ok_or(anyhow!("Tool call missing name: {}", tool_call))?;

    tracing::debug!(
        "\nTool call: {}\nargs: {}",
        &tool_call_name,
        &tool_call_args
    );

    // Call the tool and get the next completion from the result
    let tool_call_result = tools
        .iter()
        .find(|i| *i.function_name() == *tool_call_name)
        .ok_or(anyhow!(
            "Received tool call that doesn't exist: {}",
            tool_call_name
        ))?
        .call(tool_call_args)
        .await?;

    let tool_call_request = vec![FunctionCall {
        function: FunctionCallFn {
            arguments: tool_call_args.to_string(),
            name: tool_call_name.to_string(),
        },
        id: tool_call_id.to_string(),
        r#type: String::from("function"),
    }];
    let results = vec![
        Message::new_tool_call_request(tool_call_request),
        Message::new_tool_call_response(&tool_call_result, tool_call_id),
    ];

    Ok(results)
}

async fn handle_tool_calls(
    tools: &Vec<BoxedToolCall>,
    tool_calls: &[Value],
) -> Result<Vec<Message>, Error> {
    // Run each tool call concurrently and return them in order. I'm
    // not sure if ordering really matters for OpenAI compatible API
    // implementations, but better to be safe. This could also be
    // done using a `futures::stream` and `FutureUnordered` which
    // would be more efficient as it runs on the same thread, but that
    // causes lifetime issues that I don't understand how to get
    // around.
    let futures = tool_calls.iter().map(|call| handle_tool_call(tools, call));
    // Flatten the results to match what the API is expecting.
    let results = try_join_all(futures).await?.into_iter().flatten().collect();
    Ok(results)
}

/// Runs the next turn in chat by passing a transcript to the LLM for
/// the next response. Can return multiple messages when there are
/// tool calls.
pub async fn chat(
    tools: &Option<Vec<BoxedToolCall>>,
    history: &Vec<Message>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Message>, Error> {
    let mut updated_history = history.to_owned();
    let mut messages = Vec::new();

    let mut resp = completion(history, tools, api_hostname, api_key, model).await?;

    let tools_ref = tools
        .as_ref()
        .expect("Received tool call but no tools were specified");

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        if tool_calls.is_empty() {
            break;
        }

        let tool_call_msgs = handle_tool_calls(tools_ref, tool_calls).await?;
        for m in tool_call_msgs.into_iter() {
            messages.push(m.clone());
            updated_history.push(m);
        }

        // Provide the results of the tool calls back to the chat
        resp = completion(&updated_history, tools, api_hostname, api_key, model).await?;
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        messages.push(Message::new(Role::Assistant, msg));
    } else {
        panic!("No message received. Resp:\n\n {}", resp);
    }

    Ok(messages)
}

/// Runs the next turn in chat by passing a transcript to the LLM and
/// the next response is streamed via the transmitter channel
/// `tx`. Also returns the next messages so they can be processed
/// further. Can return multiple messages when there are tool calls.
pub async fn chat_stream(
    tx: mpsc::UnboundedSender<String>,
    tools: &Option<Vec<BoxedToolCall>>,
    history: &Vec<Message>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Message>, Error> {
    let mut updated_history = history.to_owned();
    let mut messages = Vec::new();

    let mut resp =
        completion_stream(tx.clone(), history, tools, api_hostname, api_key, model).await?;

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        if tool_calls.is_empty() {
            break;
        }
        let tools_ref = tools
            .as_ref()
            .expect("Received tool call but no tools were specified");

        // TODO: Update this to be streaming
        let tool_call_msgs = handle_tool_calls(tools_ref, tool_calls).await?;
        for m in tool_call_msgs.into_iter() {
            messages.push(m.clone());
            updated_history.push(m);
        }

        // Provide the results of the tool calls back to the chat
        resp = completion_stream(
            tx.clone(),
            &updated_history,
            tools,
            api_hostname,
            api_key,
            model,
        )
        .await?;
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        messages.push(Message::new(Role::Assistant, msg));
    } else {
        bail!("No message received. Resp:\n\n {}", resp);
    }

    Ok(messages)
}

pub async fn insert_chat_message(
    db: &Connection,
    session_id: &str,
    msg: &Message,
) -> Result<usize, Error> {
    let s_id = session_id.to_owned();
    let data = json!(msg).to_string();
    let result = db
        .call(move |conn| {
            let mut stmt =
                conn.prepare("INSERT INTO chat_message (session_id, data) VALUES (?, ?)")?;
            let result = stmt.execute([s_id, data])?;
            Ok(result)
        })
        .await?;

    Ok(result)
}

pub async fn get_or_create_session(
    db: &Connection,
    session_id: &str,
    tags: &[&str],
) -> Result<(), Error> {
    let session_id_owned = session_id.to_owned(); // String
    let tag_names: Vec<String> = tags
        .iter()
        .map(|s| s.to_lowercase().trim().to_string())
        .collect();

    db.call(move |conn| {
        // All tag-related database calls either all succeed or it
        // fails and rollsback to avoid inconsistent data
        let tx = conn.transaction()?;

        // Insert a new session record if it doesn't already exist
        let result = tx.execute(
            "INSERT OR IGNORE INTO session (id) VALUES (?)",
            [&session_id_owned],
        )?;
        if !tag_names.is_empty() {
            // Insert all tags first (ignore duplicates)
            for tag in &tag_names {
                tx.execute("INSERT OR IGNORE INTO tag (name) VALUES (?)", [tag.clone()])?;
            }

            // Insert all session_tag relationships using a single query approach
            for tag in &tag_names {
                // Get the tag_id for this tag
                let tag_id: i64 =
                    tx.query_row("SELECT id FROM tag WHERE name = ?", [tag.clone()], |row| {
                        row.get(0)
                    })?;

                // Insert the session_tag relationship if it doesn't already exist
                tx.execute(
                    "INSERT OR IGNORE INTO session_tag (session_id, tag_id) VALUES (?, ?)",
                    [&session_id_owned, &tag_id.to_string()],
                )?;
            }
        }

        tx.commit()?;
        Ok(result)
    })
    .await?;

    Ok(())
}

pub async fn find_chat_session_by_id(
    db: &Connection,
    session_id: &str,
) -> Result<Vec<Message>, Error> {
    let s_id = session_id.to_owned();
    let history = db.call(move |conn| {
        let mut stmt = conn.prepare("SELECT data FROM chat_message WHERE session_id=?")?;
        let rows = stmt
            .query_map([s_id], |i| {
                let val: String = i.get(0)?;
                let msg: Message = serde_json::from_str(&val).unwrap();
                Ok(msg)
            })?
            .filter_map(Result::ok)
            .collect::<Vec<Message>>();
        Ok(rows)
    });
    Ok(history.await?)
}

pub async fn chat_session_count(db: &Connection, include_tags: &[String], exclude_tags: &[String]) -> Result<i64, Error> {
    // If no filters, simple count
    if include_tags.is_empty() && exclude_tags.is_empty() {
        return db.call(|conn| {
            let mut stmt = conn.prepare("SELECT COUNT(*) FROM session")?;
            let count: i64 = stmt.query_row([], |row| row.get(0))?;
            Ok(count)
        })
        .await
        .map_err(anyhow::Error::from);
    }

    let include_json = json!(include_tags).to_string();
    let exclude_json = json!(exclude_tags).to_string();
    let inc_len = include_tags.len() as i64;
    let exc_len = exclude_tags.len() as i64;
    let count = db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                r#"
                    SELECT COUNT(*) FROM session s
                    WHERE ( ?1 = 0 OR EXISTS (
                        SELECT 1 FROM session_tag st JOIN tag t ON st.tag_id = t.id
                        WHERE st.session_id = s.id AND t.name IN (SELECT value FROM json_each(?2))
                    ))
                    AND ( ?3 = 0 OR NOT EXISTS (
                        SELECT 1 FROM session_tag st2 JOIN tag t2 ON st2.tag_id = t2.id
                        WHERE st2.session_id = s.id AND t2.name IN (SELECT value FROM json_each(?4))
                    ))
                "#,
            )?;
            let count: i64 = stmt.query_row(params![inc_len, include_json.as_bytes(), exc_len, exclude_json.as_bytes()], |row| row.get(0))?;
            Ok(count)
        })
        .await?;
    Ok(count)
}

pub async fn chat_session_list(
    db: &Connection,
    include_tags: &[String],
    exclude_tags: &[String],
    limit: usize,
    offset: usize,
) -> Result<Vec<ChatSession>, Error> {
    // If no filters, simple query without tag joins for performance
    if include_tags.is_empty() && exclude_tags.is_empty() {
        return Ok(db.call(move |conn| {
            let mut stmt = conn.prepare(
                r#"
                SELECT s.id, s.title, s.summary,
                       '' as tags
                FROM session s
                ORDER BY s.created_at DESC
                LIMIT ?1 OFFSET ?2
                "#,
            )?;
            let session_list = stmt
                .query_map(params![limit, offset], |row| {
                    Ok(ChatSession {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        summary: row.get(2)?,
                        tags: vec![],
                    })
                })?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            Ok(session_list)
        })
        .await?);
    }

    let include_json = json!(include_tags).to_string();
    let exclude_json = json!(exclude_tags).to_string();
    let inc_len = include_tags.len() as i64;
    let exc_len = exclude_tags.len() as i64;

    let results = db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                r#"
                SELECT
                    s.id,
                    s.title,
                    s.summary,
                    GROUP_CONCAT(DISTINCT t.name) as tags
                FROM session s
                LEFT JOIN session_tag st ON s.id = st.session_id
                LEFT JOIN tag t ON st.tag_id = t.id
                WHERE ( ?1 = 0 OR EXISTS (
                        SELECT 1 FROM session_tag st2 JOIN tag t2 ON st2.tag_id = t2.id
                        WHERE st2.session_id = s.id AND t2.name IN (SELECT value FROM json_each(?2))
                    ))
                  AND ( ?3 = 0 OR NOT EXISTS (
                        SELECT 1 FROM session_tag st3 JOIN tag t3 ON st3.tag_id = t3.id
                        WHERE st3.session_id = s.id AND t3.name IN (SELECT value FROM json_each(?4))
                    ))
                GROUP BY s.id, s.title, s.summary, s.created_at
                ORDER BY s.created_at DESC
                LIMIT ?5 OFFSET ?6
                "#,
            )?;
            let session_list = stmt
                .query_map(params![inc_len, include_json.as_str(), exc_len, exclude_json.as_str(), limit, offset], |row| {
                    let session_id: String = row.get(0)?;
                    let title: Option<String> = row.get(1)?;
                    let summary: Option<String> = row.get(2)?;
                    let tags_str: Option<String> = row.get(3)?;
                    let tags = match tags_str {
                        Some(tag_str) => tag_str.split(',').map(|s| s.to_string()).collect(),
                        None => vec![],
                    };
                    Ok(ChatSession { id: session_id, title, summary, tags })
                })?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            Ok(session_list)
        })
        .await
        .map_err(anyhow::Error::from)?;
    Ok(results)
}
