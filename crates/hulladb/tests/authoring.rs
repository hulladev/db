use hulladb::schema::{InsertKeyAction, KeyGeneration, RecordValues, ScalarType, Value};
use hulladb::{Codec, Id, InsertInput, PatchInput, Uuid};
use serde_json::json;

#[path = "support/model.rs"]
mod model;
use model::*;

#[derive(hulladb::Table)]
struct Tree {
    id: Id<Self>,
    parent: Option<Id<Self>>,
}

#[test]
fn authored_self_ids_remain_branded_to_the_row_in_generated_structs() {
    let schema = hulladb::schema!(Tree).unwrap();
    let reference = &schema.references()[0];
    assert_eq!(reference.target_table(), "Tree");
    let parent = Id::<Tree>::new(Uuid::now_v7());
    let input = TreeInsert {
        parent: Some(parent),
    };
    assert_eq!(
        TreeInsert::decode(&input.encode().unwrap()).unwrap().parent,
        Some(parent)
    );
    let explicit = TreeInsertWithKey {
        id: Some(parent),
        parent: None,
    };
    assert_eq!(
        explicit.values()["id"],
        Value::Uuid(*parent.key().as_bytes())
    );
    let field: hulladb::Field<Tree, Option<Id<Tree>>> = Tree::fields().parent;
    let _ = field;
}

#[test]
fn derived_schema_inserts_references_and_defaults_share_canonical_validation() {
    let schema = hulladb::schema!(Publication, Article, Counter).unwrap();
    let article = schema.table("articles").unwrap();
    assert_eq!(article.key_generation(), &KeyGeneration::UuidV4);
    let reference = &schema.references()[0];
    assert_eq!(
        (
            reference.table(),
            reference.field(),
            reference.target_table(),
            reference.target_field()
        ),
        ("articles", "publication", "publications", "slug")
    );
    assert_eq!(reference.scalar_type(), ScalarType::String);
    let input = ArticleInsert {
        publication: Id::new("daily".into()),
        title: "First".into(),
        published: None,
        excerpt: None,
        ordinal: None,
        origin: None,
    };
    let decoded = ArticleInsert::decode(&input.encode().unwrap()).unwrap();
    assert_eq!(input.values(), decoded.values());
    let prepared = article.prepare_insert(decoded.values()).unwrap();
    assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
    let mut values = prepared.into_values();
    assert_eq!(values["ordinal"], Value::Int64(i64::MIN));
    assert_eq!(values["excerpt"], Value::Null);
    assert_eq!(values["published"], Value::Boolean(false));
    values.insert("id".into(), Value::Uuid(*Uuid::now_v7().as_bytes()));
    article.validate_row(&values).unwrap();
    assert!(article.validate_insert(&values).is_err());
    assert!(Article::decode(&json!({"id": Uuid::now_v7().to_string(), "publication": "daily", "displayTitle": "First"})).is_err());
}

#[test]
fn generated_patch_preserves_omission_and_explicit_null_and_rejects_key_changes() {
    let patch = ArticlePatch {
        excerpt: Some(None),
        title: Some("Changed".into()),
        ..ArticlePatch::default()
    };
    assert_eq!(
        patch.values(),
        RecordValues::from([
            ("excerpt".into(), Value::Null),
            ("displayTitle".into(), Value::String("Changed".into())),
        ])
    );
    let encoded = patch.encode().unwrap();
    assert_eq!(
        ArticlePatch::decode(&encoded).unwrap().values(),
        patch.values()
    );
    assert!(ArticlePatch::decode(&json!({"id": Uuid::now_v7().to_string()})).is_err());
    assert!(
        ArticleInsert::decode(
            &json!({"publication": "daily", "displayTitle": "First", "origin": null})
        )
        .is_err()
    );
}

#[test]
fn derived_inputs_reject_lossy_numbers_unknown_fields_and_missing_nullable_fields() {
    let value = json!({"publication": "daily", "title": "First", "ordinal": "9223372036854775807", "notes": null});
    let parsed = CreateArticle::decode(&value).unwrap();
    assert_eq!(parsed.ordinal, i64::MAX);
    assert!(!parsed.published);
    assert_eq!(
        CreateArticle::decode(&parsed.encode().unwrap()).unwrap(),
        parsed
    );
    for ordinal in [
        json!(42),
        json!("+1"),
        json!("01"),
        json!("-0"),
        json!("9223372036854775808"),
    ] {
        let mut invalid = value.clone();
        invalid["ordinal"] = ordinal;
        assert!(CreateArticle::decode(&invalid).is_err());
    }
    let mut invalid = value.clone();
    invalid["extra"] = json!(true);
    assert!(CreateArticle::decode(&invalid).is_err());
    let mut invalid = value.clone();
    invalid.as_object_mut().unwrap().remove("notes");
    assert!(CreateArticle::decode(&invalid).is_err());
    let mut invalid = value;
    invalid["notes"] = json!(false);
    assert!(CreateArticle::decode(&invalid).is_err());
}

#[test]
fn custom_non_uuid_key_inputs_preserve_advanced_explicit_insert_policy() {
    let schema = hulladb::schema!(Counter).unwrap();
    let table = schema.table("counters").unwrap();
    let generated = CounterInsert { value: i64::MIN };
    assert_eq!(
        table.validate_insert(&generated.values()).unwrap(),
        InsertKeyAction::Generate
    );
    let explicit = CounterInsertWithKey {
        sequence: Some(Id::new(i64::MAX)),
        value: i64::MIN,
    };
    assert_eq!(
        table.validate_insert(&explicit.values()).unwrap(),
        InsertKeyAction::Provided
    );
    let encoded = explicit.encode().unwrap();
    assert_eq!(encoded["sequence"], "9223372036854775807");
    assert_eq!(
        CounterInsertWithKey::decode(&encoded).unwrap().values(),
        explicit.values()
    );
    assert!(PublicationInsert::decode(&json!({"title": "missing key"})).is_err());
}
