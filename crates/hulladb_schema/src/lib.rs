//! Language-neutral schema contracts for Hulla DB.
//!
//! Authoring frontends produce [`TableDeclaration`] values. Normalization resolves
//! the primary key and adds the implicit UUID `id` only when needed. These are
//! internal compiler contracts; the application-facing Rust and TypeScript APIs
//! remain provisional.
//!
//! ```
//! use hulladb_schema::{FieldDefinition, KeyGeneration, ScalarType, TableDeclaration};
//!
//! let projects = TableDeclaration::new(
//!     "projects",
//!     vec![FieldDefinition::new("name", ScalarType::String)],
//! ).normalize()?;
//!
//! assert_eq!(projects.primary_key().name, "id");
//! assert_eq!(projects.primary_key().scalar_type, ScalarType::Uuid);
//! assert_eq!(projects.key_generation(), &KeyGeneration::UuidV7);
//!
//! let mut articles = TableDeclaration::new(
//!     "articles",
//!     vec![FieldDefinition::new("slug", ScalarType::String)],
//! );
//! articles.primary_key = Some("slug".into());
//! let articles = articles.normalize()?;
//!
//! assert_eq!(articles.primary_key().name, "slug");
//! assert!(articles.field("id").is_none());
//! assert_eq!(articles.key_generation(), &KeyGeneration::None);
//! # Ok::<(), hulladb_schema::SchemaError>(())
//! ```
//!
//! Generation and insert policy are configured independently:
//!
//! ```
//! use hulladb_schema::{
//!     FieldDefinition, InsertKeyPolicy, KeyGeneration, ScalarType, TableDeclaration,
//! };
//!
//! let mut tasks = TableDeclaration::new(
//!     "tasks",
//!     vec![FieldDefinition::new("id", ScalarType::Int64)],
//! );
//! tasks.key_generation = Some(KeyGeneration::Custom {
//!     handler: "tasks.next_id".into(),
//!     output_type: ScalarType::Int64,
//! });
//! tasks.insert_key_policy = InsertKeyPolicy::GeneratedOnly;
//! let tasks = tasks.normalize()?;
//!
//! assert_eq!(tasks.insert_key_policy(), InsertKeyPolicy::GeneratedOnly);
//! # Ok::<(), hulladb_schema::SchemaError>(())
//! ```
//!
//! Normalization checks the declared generator output type. Resolving the server
//! handler and executing generation require later compiler/runtime stages.
//! Produced values must pass the complete-row validator before storage.
//!
//! Validate decoded insert values before generation, then the complete row:
//!
//! ```
//! use hulladb_schema::{
//!     FieldDefinition, InsertKeyAction, RecordValues, ScalarType, TableDeclaration, Value,
//! };
//!
//! let mut notes = FieldDefinition::new("notes", ScalarType::String);
//! notes.nullable = true;
//! let tasks = TableDeclaration::new("tasks", vec![notes]).normalize()?;
//! let mut values = RecordValues::from([("notes".into(), Value::Null)]);
//! assert_eq!(tasks.validate_insert(&values)?, InsertKeyAction::Generate);
//! assert!(tasks.validate_row(&values).is_err()); // A complete row needs its key.
//!
//! // Stand-in for the key a future runtime generator would produce.
//! values.insert("id".into(), Value::Uuid([1; 16]));
//! tasks.validate_row(&values)?;
//! values.remove("notes");
//! assert!(tasks.validate_insert(&values).is_err()); // Nullable is still required.
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Partial updates preserve omissions and reject primary-key assignments:
//!
//! ```
//! use hulladb_schema::{FieldDefinition, RecordValues, ScalarType, TableDeclaration, Value};
//!
//! let mut notes = FieldDefinition::new("notes", ScalarType::String);
//! notes.nullable = true;
//! let tasks = TableDeclaration::new("tasks", vec![notes]).normalize()?;
//! let mut row = RecordValues::from([
//!     ("id".into(), Value::Uuid([1; 16])),
//!     ("notes".into(), Value::String("Old notes".into())),
//! ]);
//! tasks.apply_update(&mut row, RecordValues::from([("notes".into(), Value::Null)]))?;
//! assert_eq!(row.get("notes"), Some(&Value::Null));
//! assert_eq!(row.get("id"), Some(&Value::Uuid([1; 16])));
//! assert!(tasks.validate_update(&RecordValues::from([
//!     ("id".into(), Value::Uuid([1; 16])),
//! ])).is_err());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Literal defaults permit omission only during insertion:
//!
//! ```
//! use hulladb_schema::{
//!     FieldDefinition, InsertKeyAction, RecordValues, ScalarType, TableDeclaration, Value,
//! };
//!
//! let mut notes = FieldDefinition::new("notes", ScalarType::String);
//! notes.nullable = true;
//! notes.default = Some(Value::Null);
//! let mut done = FieldDefinition::new("done", ScalarType::Boolean);
//! done.default = Some(Value::Boolean(false));
//! let tasks = TableDeclaration::new("tasks", vec![notes, done]).normalize()?;
//!
//! let prepared = tasks.prepare_insert(RecordValues::new())?;
//! assert_eq!(prepared.values().get("notes"), Some(&Value::Null));
//! assert_eq!(prepared.values().get("done"), Some(&Value::Boolean(false)));
//! assert_eq!(prepared.key_action(), InsertKeyAction::Generate);
//! let mut values = prepared.into_values();
//! values.insert("id".into(), Value::Uuid([1; 16])); // Future runtime generation.
//! tasks.validate_row(&values)?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

//! Resolve explicit references across tables:
//!
//! ```
//! use hulladb_schema::{
//!     FieldDefinition, ReferenceDeclaration, ScalarType, SchemaDeclaration, TableDeclaration,
//! };
//!
//! let projects = TableDeclaration::new("projects", vec![
//!     FieldDefinition::new("id", ScalarType::String),
//! ]);
//! let tasks = TableDeclaration::new("tasks", vec![
//!     FieldDefinition::new("projectId", ScalarType::String),
//! ]);
//! let mut schema = SchemaDeclaration::new(vec![tasks, projects]);
//! schema.references.push(ReferenceDeclaration::new("tasks", "projectId", "projects"));
//! let schema = schema.normalize()?;
//! assert_eq!(schema.references()[0].target_field(), "id");
//! assert_eq!(schema.references()[0].scalar_type(), ScalarType::String);
//! // Storage must enforce target existence and restrict referenced-row deletion.
//! # Ok::<(), hulladb_schema::SchemaError>(())
//! ```

//! Choose a schema UUID default and override individual tables:
//!
//! ```
//! use hulladb_schema::{KeyGeneration, SchemaDeclaration, TableDeclaration, UuidGeneration};
//!
//! let projects = TableDeclaration::new("projects", vec![]);
//! let mut tasks = TableDeclaration::new("tasks", vec![]);
//! tasks.key_generation = Some(KeyGeneration::UuidV7);
//! let mut schema = SchemaDeclaration::new(vec![projects, tasks]);
//! schema.default_uuid_generation = UuidGeneration::V4;
//! let schema = schema.normalize()?;
//! assert_eq!(schema.table("projects").unwrap().key_generation(), &KeyGeneration::UuidV4);
//! assert_eq!(schema.table("tasks").unwrap().key_generation(), &KeyGeneration::UuidV7);
//! # Ok::<(), hulladb_schema::SchemaError>(())
//! ```

//! Describe business uniqueness separately from row identity:
//!
//! ```
//! use hulladb_schema::{
//!     FieldDefinition, ScalarType, TableDeclaration, UniqueConstraintDeclaration, UniqueNullPolicy,
//! };
//!
//! let mut memberships = TableDeclaration::new("memberships", vec![
//!     FieldDefinition::new("projectId", ScalarType::Uuid),
//!     FieldDefinition::new("userId", ScalarType::Uuid),
//! ]);
//! memberships.unique_constraints.push(UniqueConstraintDeclaration::new(
//!     "project_user", vec!["projectId".into(), "userId".into()],
//! ));
//! let memberships = memberships.normalize()?;
//! assert_eq!(memberships.primary_key().name, "id");
//! assert_eq!(memberships.unique_constraints()[0].fields(), &["projectId", "userId"]);
//!
//! // Nullable unique fields require an explicit null comparison policy.
//! let mut email = FieldDefinition::new("email", ScalarType::String);
//! email.nullable = true;
//! let mut users = TableDeclaration::new("users", vec![email]);
//! let mut constraint = UniqueConstraintDeclaration::new("email", vec!["email".into()]);
//! constraint.null_policy = Some(UniqueNullPolicy::Distinct); // Allow repeated nulls.
//! users.unique_constraints.push(constraint);
//! let users = users.normalize()?;
//! assert_eq!(users.unique_constraints()[0].null_policy(), UniqueNullPolicy::Distinct);
//! // Storage transactions must enforce uniqueness; normalization checks declarations.
//! # Ok::<(), hulladb_schema::SchemaError>(())
//! ```

//! Validate an explicit entity projection without treating it as a full row:
//!
//! ```
//! use hulladb_schema::{FieldDefinition, RecordValues, ScalarType, TableDeclaration, Value};
//! let tasks = TableDeclaration::new("tasks", vec![
//!     FieldDefinition::new("title", ScalarType::String),
//!     FieldDefinition::new("version", ScalarType::Int64),
//! ]).normalize()?;
//! let summary = tasks.projection("summary", &["id", "title"])?;
//! let values = RecordValues::from([
//!     ("id".into(), Value::Uuid([1; 16])),
//!     ("title".into(), Value::String("Ship".into())),
//! ]);
//! summary.validate_record(&values)?;
//! assert!(tasks.validate_row(&values).is_err()); // A full row also needs version.
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod constraints;
mod error;
mod json;
mod key;
mod projection;
mod schema;
mod table;
pub mod temporal;
mod types;
mod unique;
mod validation;
mod value;

pub use error::SchemaError;
pub use json::{JsonProperty, JsonShape};
pub use key::{InsertKeyPolicy, KeyGeneration, UuidGeneration};
pub use projection::{ProjectionContract, ProjectionError, ProjectionErrorKind};
pub use schema::{
    ReferenceContract, ReferenceDeclaration, ReferenceOnDelete, SchemaContract, SchemaDeclaration,
};
pub use table::{TableContract, TableDeclaration};
pub use types::{FieldDefinition, ScalarType};
pub use unique::{UniqueConstraintContract, UniqueConstraintDeclaration, UniqueNullPolicy};
pub use validation::{InsertKeyAction, PreparedInsert, ValidationError, ValidationErrorKind};
pub use value::{RecordValues, Value};

pub use constraints::{CheckDefinition, CheckOperand, FieldConstraints, IndexDeclaration};
