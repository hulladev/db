use hulladb::{Id, Table};

#[derive(Table)]
struct Owner { id: Id<Owner> }
#[derive(Table)]
struct Other { id: Id<Other> }
#[derive(Table)]
struct Note {
    id: Id<Note>,
    #[field(reference = Other)]
    owner: Id<Owner>,
}
fn main() {}
