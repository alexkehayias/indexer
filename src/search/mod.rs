pub mod aql;
mod core;
pub mod fts;
pub use fts::utils::recreate_index;
mod indexing;
pub use indexing::{
    delete_chat_session_index, index_all, index_all_chat_sessions, index_chat_messages,
    index_single_file, remove_task_from_indexes,
};
mod query;
mod source;
pub use core::search_notes;
