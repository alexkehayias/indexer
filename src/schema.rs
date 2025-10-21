use tantivy::schema::*;

pub fn note_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("id", TEXT | STORED);
    schema_builder.add_text_field("type", TEXT | STORED);
    schema_builder.add_text_field("category", TEXT | STORED);
    schema_builder.add_text_field("title", TEXT | STORED);
    schema_builder.add_text_field("tags", TEXT | STORED);
    schema_builder.add_text_field("status", TEXT | STORED);
    schema_builder.add_text_field("body", TEXT | STORED);
    schema_builder.add_text_field("file_name", TEXT | STORED);
    schema_builder.build()
}
