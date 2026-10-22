use hulladb::{Id, Input, Table, Uuid};

#[derive(Table, Debug)]
#[table(name = "publications", key = "slug")]
pub struct Publication {
    pub slug: String,
    pub title: String,
}

#[derive(Table, Debug)]
#[table(
    name = "articles",
    generation = "uuidV4",
    insert_key_policy = "generatedOnly"
)]
#[unique(name = "publication_title", fields(publication, title))]
#[unique(name = "excerpt", fields(excerpt), nulls = "distinct")]
pub struct Article {
    pub id: Id<Article>,
    pub publication: Id<Publication>,
    #[field(name = "displayTitle")]
    pub title: String,
    #[field(default = false)]
    pub published: bool,
    #[field(default = None)]
    pub excerpt: Option<String>,
    #[field(default = -9223372036854775808)]
    pub ordinal: i64,
    #[field(default = "AAAAAAAA-0000-0000-0000-000000000001")]
    pub origin: Uuid,
}

#[derive(Table, Debug)]
#[table(name = "counters", key = "sequence", key_type = i64, generator = "counters.next")]
pub struct Counter {
    pub sequence: Id<Counter>,
    pub value: i64,
}

#[derive(Input, Debug, PartialEq)]
pub struct CreateArticle {
    pub publication: Id<Publication>,
    pub title: String,
    pub ordinal: i64,
    pub notes: Option<String>,
    #[field(default = false)]
    pub published: bool,
}
