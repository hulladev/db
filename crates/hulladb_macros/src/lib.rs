use proc_macro::TokenStream;
use proc_macro2::TokenStream as Tokens;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, Fields, FnArg, GenericArgument, Ident, ItemFn, LitStr, PathArguments,
    ReturnType, Type, parse_macro_input, spanned::Spanned,
};

#[proc_macro_derive(Table, attributes(table, field, unique))]
pub fn table(input: TokenStream) -> TokenStream {
    emit(derive_table(parse_macro_input!(input as DeriveInput)))
}
#[proc_macro_derive(Input, attributes(field))]
pub fn input(input: TokenStream) -> TokenStream {
    emit(derive_input(parse_macro_input!(input as DeriveInput)))
}
#[proc_macro_attribute]
pub fn query(args: TokenStream, item: TokenStream) -> TokenStream {
    emit(function(
        args.into(),
        parse_macro_input!(item as ItemFn),
        false,
    ))
}
#[proc_macro_attribute]
pub fn mutation(args: TokenStream, item: TokenStream) -> TokenStream {
    emit(function(
        args.into(),
        parse_macro_input!(item as ItemFn),
        true,
    ))
}
fn emit(result: syn::Result<Tokens>) -> TokenStream {
    result.unwrap_or_else(syn::Error::into_compile_error).into()
}

struct Field {
    ident: Ident,
    ty: Type,
    name: String,
    default: Option<Expr>,
    reference: Option<Type>,
}
fn argument(ty: &Type, name: &str) -> Option<Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != name {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(ty) if args.args.len() == 1 => Some(ty.clone()),
        _ => None,
    }
}
fn scalar(ty: &Type) -> syn::Result<()> {
    if let Some(inner) = argument(ty, "Option") {
        if argument(&inner, "Option").is_some() {
            return Err(syn::Error::new(
                ty.span(),
                "nested Option is not an authored scalar",
            ));
        }
        return scalar(&inner);
    }
    if argument(ty, "Id").is_some() {
        return Ok(());
    }
    if let Type::Path(path) = ty {
        let name = path
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        if ["String", "bool", "i64", "Uuid"].contains(&name.as_str()) {
            return Ok(());
        }
    }
    Err(syn::Error::new(
        ty.span(),
        "supported scalars are String, bool, i64, Uuid, Id<Table>, and Option of those",
    ))
}
fn fields(input: &DeriveInput) -> syn::Result<Vec<Field>> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            input.generics.span(),
            "authored table/input structs cannot be generic",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "derive requires a named-field struct",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new(
            data.fields.span(),
            "derive requires a named-field struct",
        ));
    };
    let mut names = std::collections::BTreeSet::new();
    fields
        .named
        .iter()
        .map(|field| {
            scalar(&field.ty)?;
            let ident = field.ident.clone().expect("named field");
            let mut name = ident.to_string().trim_start_matches("r#").to_owned();
            let mut default = None;
            let mut reference = None;
            let mut options = std::collections::BTreeSet::new();
            for attr in field.attrs.iter().filter(|a| a.path().is_ident("field")) {
                attr.parse_nested_meta(|meta| {
                    if !options.insert(meta.path.clone().into_token_stream().to_string()) {
                        return Err(meta.error("duplicate field option"));
                    }
                    if meta.path.is_ident("name") {
                        name = meta.value()?.parse::<LitStr>()?.value();
                    } else if meta.path.is_ident("default") {
                        default = Some(meta.value()?.parse()?);
                    } else if meta.path.is_ident("reference") {
                        reference = Some(meta.value()?.parse()?);
                    } else {
                        return Err(meta.error("unknown field option"));
                    }
                    Ok(())
                })?;
            }
            if name.is_empty() || !names.insert(name.clone()) {
                return Err(syn::Error::new(
                    field.span(),
                    "empty or duplicate logical field name",
                ));
            }
            Ok(Field {
                ident,
                ty: field.ty.clone(),
                name,
                default,
                reference,
            })
        })
        .collect()
}
use quote::ToTokens;

fn default_value(ty: &Type, expr: &Expr) -> syn::Result<Tokens> {
    if let Some(inner) = argument(ty, "Option") {
        if matches!(expr, Expr::Path(path) if path.path.is_ident("None")) {
            return Ok(quote!(None));
        }
        let inner = default_value(&inner, expr)?;
        return Ok(quote!(Some(#inner)));
    }
    let Type::Path(path) = ty else {
        return Err(syn::Error::new(ty.span(), "unsupported default"));
    };
    let name = path
        .path
        .segments
        .last()
        .expect("scalar path")
        .ident
        .to_string();
    match (name.as_str(), expr) {
        ("String", Expr::Lit(lit)) if matches!(lit.lit, syn::Lit::Str(_)) => {
            Ok(quote!(#expr.to_owned()))
        }
        ("bool", Expr::Lit(lit)) if matches!(lit.lit, syn::Lit::Bool(_)) => Ok(quote!(#expr)),
        ("i64", Expr::Lit(lit)) if matches!(lit.lit, syn::Lit::Int(_)) => Ok(quote!(#expr)),
        ("i64", Expr::Unary(unary))
            if matches!(unary.op, syn::UnOp::Neg(_))
                && matches!(unary.expr.as_ref(), Expr::Lit(lit) if matches!(lit.lit, syn::Lit::Int(_))) =>
        {
            Ok(quote!(#expr))
        }
        ("Uuid", Expr::Lit(lit)) => {
            let syn::Lit::Str(text) = &lit.lit else {
                return Err(syn::Error::new(
                    expr.span(),
                    "UUID defaults require a hyphenated UUID string literal",
                ));
            };
            let text = text.value();
            if text.len() != 36 || ![8, 13, 18, 23].iter().all(|i| text.as_bytes()[*i] == b'-') {
                return Err(syn::Error::new(expr.span(), "invalid UUID default"));
            }
            let digits: String = text.chars().filter(|c| *c != '-').collect();
            let mut bytes = Vec::new();
            for i in (0..32).step_by(2) {
                let pair = digits
                    .get(i..i + 2)
                    .ok_or_else(|| syn::Error::new(expr.span(), "invalid UUID default"))?;
                bytes.push(
                    u8::from_str_radix(pair, 16)
                        .map_err(|_| syn::Error::new(expr.span(), "invalid UUID default"))?,
                );
            }
            Ok(quote!(::hulladb::Uuid::from_bytes([#(#bytes),*])))
        }
        _ => Err(syn::Error::new(
            expr.span(),
            "defaults must be scalar literals (or None for a nullable field)",
        )),
    }
}
fn metadata(field: &Field) -> syn::Result<Tokens> {
    let ty = &field.ty;
    let name = &field.name;
    let default = if let Some(default) = &field.default {
        let value = default_value(ty, default)?;
        quote!({ let value: #ty = #value; Some(::hulladb::Scalar::value(&value)) })
    } else {
        quote!(None)
    };
    Ok(
        quote!(::hulladb::schema::FieldDefinition { constraints: ::hulladb::schema::FieldConstraints::default(), name: #name.into(), scalar_type: <#ty as ::hulladb::Scalar>::TYPE, nullable: <#ty as ::hulladb::Scalar>::NULLABLE, default: #default, precision: None, json_shape: None }),
    )
}
fn codec(name: &Ident, fields: &[Field], defaults: bool) -> syn::Result<Tokens> {
    let encode = fields.iter().map(|field| {
        let ident = &field.ident;
        let name = &field.name;
        quote!(object.insert(#name.into(), ::hulladb::Codec::encode(&self.#ident)?);)
    });
    let decode = fields.iter().map(|field| {
        let ident = &field.ident; let name = &field.name; let ty = &field.ty;
        if defaults && let Some(default) = &field.default {
            let default = default_value(ty, default)?;
            Ok(quote!(let #ident: #ty = match object.remove(#name) { Some(value) => <#ty as ::hulladb::Codec>::decode(&value)?, None => #default };))
        } else {
            Ok(quote!(let #ident: #ty = <#ty as ::hulladb::Codec>::decode(&::hulladb::__private::required(&mut object, #name)?)?;))
        }
    }).collect::<syn::Result<Vec<_>>>()?;
    let names = fields.iter().map(|field| &field.ident);
    Ok(quote! {
        impl ::hulladb::Codec for #name {
            fn encode(&self) -> Result<::hulladb::__private::Json, ::hulladb::Error> {
                let mut object = ::hulladb::__private::Map::new(); #(#encode)* Ok(::hulladb::__private::Json::Object(object))
            }
            fn decode(value: &::hulladb::__private::Json) -> Result<Self, ::hulladb::Error> {
                let mut object = ::hulladb::__private::object(value)?; #(#decode)*
                ::hulladb::__private::reject_unknown(&object)?; Ok(Self { #(#names),* })
            }
        }
    })
}
fn derive_input(input: DeriveInput) -> syn::Result<Tokens> {
    let fields = fields(&input)?;
    if fields.iter().any(|f| f.reference.is_some()) {
        return Err(syn::Error::new(
            input.span(),
            "input references use Id<Table>; field(reference) is a table constraint",
        ));
    }
    let name = &input.ident;
    let codec = codec(name, &fields, true)?;
    let metadata = fields
        .iter()
        .map(metadata)
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(
        quote!(#codec impl ::hulladb::Input for #name { fn fields() -> Vec<::hulladb::schema::FieldDefinition> { vec![#(#metadata),*] } }),
    )
}

fn derive_table(input: DeriveInput) -> syn::Result<Tokens> {
    let name = &input.ident;
    let mut fields = fields(&input)?;
    for field in &mut fields {
        resolve_self(&mut field.ty, name);
        if let Some(reference) = &mut field.reference {
            resolve_self(reference, name);
        }
    }
    let vis = &input.vis;
    let mut logical_name = name.to_string();
    let mut key = "id".to_owned();
    let mut key_type: Option<Type> = None;
    let mut generation = None;
    let mut generator = None;
    let mut insert_policy = "allowExplicit".to_owned();
    let mut options = std::collections::BTreeSet::new();
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("table")) {
        attr.parse_nested_meta(|meta| {
            if !options.insert(meta.path.clone().into_token_stream().to_string()) {
                return Err(meta.error("duplicate table option"));
            }
            if meta.path.is_ident("name") {
                logical_name = meta.value()?.parse::<LitStr>()?.value();
            } else if meta.path.is_ident("key") {
                key = meta.value()?.parse::<LitStr>()?.value();
            } else if meta.path.is_ident("key_type") {
                key_type = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("generation") {
                generation = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("generator") {
                generator = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("insert_key_policy") {
                insert_policy = meta.value()?.parse::<LitStr>()?.value();
            } else {
                return Err(meta.error("unknown table option"));
            }
            Ok(())
        })?;
    }
    if logical_name.is_empty() {
        return Err(syn::Error::new(input.span(), "table name cannot be empty"));
    }
    for field in &fields {
        let inner = argument(&field.ty, "Option").unwrap_or_else(|| field.ty.clone());
        if field.reference.is_some() && argument(&inner, "Id").is_some() {
            return Err(syn::Error::new(
                field.ty.span(),
                "Id<Table> already declares its reference target; field(reference) is for primitive scalars",
            ));
        }
    }
    let key_field = fields.iter().find(|f| f.ident == key).ok_or_else(|| {
        syn::Error::new(
            input.span(),
            "Table requires an explicit non-null primary-key field",
        )
    })?;
    if key_field.default.is_some()
        || key_field.reference.is_some()
        || argument(&key_field.ty, "Option").is_some()
    {
        return Err(syn::Error::new(
            key_field.ty.span(),
            "primary keys cannot be nullable, defaulted, or references",
        ));
    }
    let branded = argument(&key_field.ty, "Id");
    if key_type.is_some() && branded.is_none() {
        return Err(syn::Error::new(
            key_field.ty.span(),
            "key_type is only needed for a branded Id primary key; primitive keys use their authored type",
        ));
    }
    if let Some(target) = &branded
        && !matches!(target, Type::Path(p) if p.path.is_ident(name) || p.path.is_ident("Self"))
    {
        return Err(syn::Error::new(
            target.span(),
            "a primary Id must name its own table",
        ));
    }
    let key_type = key_type.unwrap_or_else(|| {
        if branded.is_some() {
            syn::parse_quote!(::hulladb::Uuid)
        } else {
            key_field.ty.clone()
        }
    });
    scalar(&key_type)?;
    if argument(&key_type, "Option").is_some() || argument(&key_type, "Id").is_some() {
        return Err(syn::Error::new(
            key_type.span(),
            "key_type must be a non-null primitive scalar",
        ));
    }
    let generated = generator.is_some()
        || match generation.as_deref() {
            Some("none") => false,
            Some(_) => true,
            None => {
                matches!(&key_type, Type::Path(path) if path.path.segments.last().is_some_and(|s| s.ident == "Uuid"))
            }
        };
    let generation_tokens = if let Some(handler) = generator {
        if generation.is_some() || handler.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "custom generator must have a name and cannot also set generation",
            ));
        }
        quote!(Some(::hulladb::schema::KeyGeneration::Custom { handler: #handler.into(), output_type: <#key_type as ::hulladb::Scalar>::TYPE }))
    } else {
        match generation.as_deref() {
            None => quote!(None),
            Some("none") => quote!(Some(::hulladb::schema::KeyGeneration::None)),
            Some("uuidV4") => quote!(Some(::hulladb::schema::KeyGeneration::UuidV4)),
            Some("uuidV7") => quote!(Some(::hulladb::schema::KeyGeneration::UuidV7)),
            _ => {
                return Err(syn::Error::new(
                    input.span(),
                    "generation must be none, uuidV4, or uuidV7",
                ));
            }
        }
    };
    let policy = match insert_policy.as_str() {
        "allowExplicit" => quote!(::hulladb::schema::InsertKeyPolicy::AllowExplicit),
        "generatedOnly" if generated => quote!(::hulladb::schema::InsertKeyPolicy::GeneratedOnly),
        _ => {
            return Err(syn::Error::new(
                input.span(),
                "insert_key_policy must allow explicit keys or require a generator",
            ));
        }
    };
    let metadata = fields
        .iter()
        .map(metadata)
        .collect::<syn::Result<Vec<_>>>()?;
    let references = fields.iter().filter(|f| f.ident != key).filter_map(|field| {
        let inner = argument(&field.ty, "Option").unwrap_or_else(|| field.ty.clone());
        field.reference.clone().or_else(|| argument(&inner, "Id")).map(|target| { let field_name = &field.name; quote!(::hulladb::schema::ReferenceDeclaration::new(#logical_name, #field_name, <#target as ::hulladb::Table>::NAME)) })
    });
    let mut unique = Vec::new();
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("unique")) {
        let mut unique_name = None;
        let mut selected = Vec::new();
        let mut nulls = None;
        let mut options = std::collections::BTreeSet::new();
        attr.parse_nested_meta(|meta| {
            if !options.insert(meta.path.clone().into_token_stream().to_string()) {
                return Err(meta.error("duplicate unique option"));
            }
            if meta.path.is_ident("name") {
                unique_name = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("nulls") {
                nulls = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("fields") {
                let content;
                syn::parenthesized!(content in meta.input);
                selected = content
                    .parse_terminated(|input| input.parse::<Ident>(), syn::Token![,])?
                    .into_iter()
                    .collect();
            } else {
                return Err(meta.error("unknown unique option"));
            }
            Ok(())
        })?;
        let unique_name = unique_name
            .ok_or_else(|| syn::Error::new(attr.span(), "unique constraint requires name"))?;
        let selected = selected
            .iter()
            .map(|ident| {
                fields
                    .iter()
                    .find(|f| f.ident == *ident)
                    .map(|f| f.name.clone())
                    .ok_or_else(|| {
                        syn::Error::new(
                            ident.span(),
                            "unique constraint references an unknown field",
                        )
                    })
            })
            .collect::<syn::Result<Vec<_>>>()?;
        let nulls = match nulls.as_deref() {
            None => quote!(None),
            Some("distinct") => quote!(Some(::hulladb::schema::UniqueNullPolicy::Distinct)),
            Some("notDistinct") => quote!(Some(::hulladb::schema::UniqueNullPolicy::NotDistinct)),
            _ => {
                return Err(syn::Error::new(
                    attr.span(),
                    "nulls must be distinct or notDistinct",
                ));
            }
        };
        unique.push(quote!(::hulladb::schema::UniqueConstraintDeclaration { name: #unique_name.into(), fields: vec![#(#selected.into()),*], null_policy: #nulls }));
    }
    let row_codec = codec(name, &fields, false)?;
    let insert = format_ident!("{name}Insert");
    let patch = format_ident!("{name}Patch");
    let selectors = format_ident!("{name}Fields");
    let insert_fields: Vec<_> = fields
        .iter()
        .filter(|f| !generated || f.ident != key)
        .collect();
    let insert_tokens = input_struct(&insert, name, vis, &insert_fields, false, None)?;
    let patch_fields: Vec<_> = fields.iter().filter(|f| f.ident != key).collect();
    let patch_tokens = input_struct(&patch, name, vis, &patch_fields, true, None)?;
    let advanced = if generated && insert_policy == "allowExplicit" {
        let explicit = format_ident!("{name}InsertWithKey");
        input_struct(
            &explicit,
            name,
            vis,
            &fields.iter().collect::<Vec<_>>(),
            false,
            Some(&key_field.ident),
        )?
    } else {
        quote!()
    };
    let field_members = fields.iter().map(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        quote!(pub #ident: ::hulladb::Field<#name, #ty>)
    });
    let field_values = fields.iter().map(|f| {
        let ident = &f.ident;
        let logical = &f.name;
        quote!(#ident: ::hulladb::Field::new(#logical))
    });
    let key_ident = &key_field.ident;
    let key_name = &key_field.name;
    let id = if branded.is_some() {
        quote!(self.#key_ident.clone())
    } else {
        quote!(::hulladb::Id::new(self.#key_ident.clone()))
    };
    Ok(quote! {
        #row_codec #insert_tokens #patch_tokens #advanced
        #vis struct #selectors { #(#field_members),* }
        impl #name { pub fn fields() -> #selectors { #selectors { #(#field_values),* } } }
        impl ::hulladb::Table for #name {
            const NAME: &'static str = #logical_name;
            type Key = #key_type; type Insert = #insert; type Patch = #patch;
            fn declaration() -> ::hulladb::schema::TableDeclaration {
                let mut table = ::hulladb::schema::TableDeclaration::new(#logical_name, vec![#(#metadata),*]);
                table.primary_key = Some(#key_name.into()); table.key_generation = #generation_tokens;
                table.insert_key_policy = #policy; table.unique_constraints = vec![#(#unique),*]; table
            }
            fn references() -> Vec<::hulladb::schema::ReferenceDeclaration> { vec![#(#references),*] }
            fn id(&self) -> ::hulladb::Id<Self> { #id }
        }
    })
}

// Generated input/selector structs have their own Self. Resolve authored table
// Self before moving field types into those structs (including self references).
fn resolve_self(ty: &mut Type, table: &Ident) {
    if let Type::Path(path) = ty {
        if path.path.is_ident("Self") {
            *ty = syn::parse_quote!(#table);
            return;
        }
        for segment in &mut path.path.segments {
            if let PathArguments::AngleBracketed(arguments) = &mut segment.arguments {
                for argument in &mut arguments.args {
                    if let GenericArgument::Type(ty) = argument {
                        resolve_self(ty, table);
                    }
                }
            }
        }
    }
}

fn input_struct(
    name: &Ident,
    table: &Ident,
    vis: &syn::Visibility,
    fields: &[&Field],
    patch: bool,
    optional_key: Option<&Ident>,
) -> syn::Result<Tokens> {
    let optional: Vec<_> = fields
        .iter()
        .map(|f| patch || f.default.is_some() || optional_key == Some(&f.ident))
        .collect();
    let members = fields.iter().zip(&optional).map(|(f, optional)| {
        let ident = &f.ident;
        let ty = &f.ty;
        if *optional {
            quote!(pub #ident: Option<#ty>)
        } else {
            quote!(pub #ident: #ty)
        }
    });
    let encode = fields.iter().zip(&optional).map(|(f, optional)| { let ident=&f.ident; let logical=&f.name; if *optional { quote!(if let Some(value) = &self.#ident { object.insert(#logical.into(), ::hulladb::Codec::encode(value)?); }) } else { quote!(object.insert(#logical.into(), ::hulladb::Codec::encode(&self.#ident)?);) } });
    let decode = fields.iter().zip(&optional).map(|(f, optional)| { let ident=&f.ident; let logical=&f.name; let ty=&f.ty; if *optional { quote!(let #ident = object.remove(#logical).as_ref().map(<#ty as ::hulladb::Codec>::decode).transpose()?;) } else { quote!(let #ident = <#ty as ::hulladb::Codec>::decode(&::hulladb::__private::required(&mut object, #logical)?)?;) } });
    let values = fields.iter().zip(&optional).map(|(f, optional)| { let ident=&f.ident; let logical=&f.name; if *optional { quote!(if let Some(value) = &self.#ident { values.insert(#logical.into(), ::hulladb::Scalar::value(value)); }) } else { quote!(values.insert(#logical.into(), ::hulladb::Scalar::value(&self.#ident));) } });
    let names = fields.iter().map(|f| &f.ident);
    let trait_name = if patch {
        quote!(::hulladb::PatchInput)
    } else {
        quote!(::hulladb::InsertInput)
    };
    let derive_default = if optional.iter().all(|o| *o) {
        quote!(, Default)
    } else {
        quote!()
    };
    Ok(quote! {
        #[derive(Clone, Debug #derive_default)] #vis struct #name { #(#members),* }
        impl ::hulladb::Codec for #name {
            fn encode(&self) -> Result<::hulladb::__private::Json, ::hulladb::Error> { let mut object=::hulladb::__private::Map::new(); #(#encode)* Ok(::hulladb::__private::Json::Object(object)) }
            fn decode(value:&::hulladb::__private::Json) -> Result<Self,::hulladb::Error> { let mut object=::hulladb::__private::object(value)?; #(#decode)* ::hulladb::__private::reject_unknown(&object)?; Ok(Self { #(#names),* }) }
        }
        impl #trait_name for #name { type Table=#table; fn values(&self)->::hulladb::schema::RecordValues { let mut values=::hulladb::schema::RecordValues::new(); #(#values)* values } }
    })
}

fn function(args: Tokens, mut item: ItemFn, mutation: bool) -> syn::Result<Tokens> {
    let mut operation = None;
    let parser = syn::meta::parser(|meta| {
        if meta.path.is_ident("name") && operation.is_none() {
            operation = Some(meta.value()?.parse::<LitStr>()?);
            Ok(())
        } else {
            Err(meta.error("function accepts only one name option"))
        }
    });
    syn::parse::Parser::parse2(parser, args)?;
    if operation
        .as_ref()
        .is_some_and(|name| name.value().is_empty())
    {
        return Err(syn::Error::new(
            item.sig.span(),
            "function name cannot be empty",
        ));
    }
    if item.sig.asyncness.is_none()
        || !item.sig.generics.params.is_empty()
        || item.sig.generics.where_clause.is_some()
        || item.sig.constness.is_some()
        || item.sig.unsafety.is_some()
        || item.sig.abi.is_some()
        || item.sig.variadic.is_some()
    {
        return Err(syn::Error::new(
            item.sig.span(),
            "function must be an ordinary non-generic async function",
        ));
    }
    let args: Vec<_> = item.sig.inputs.iter().collect();
    if args.is_empty() || args.len() > 2 {
        return Err(syn::Error::new(
            item.sig.span(),
            "function takes its context and optionally one derived input",
        ));
    }
    let FnArg::Typed(context) = args[0] else {
        return Err(syn::Error::new(
            item.sig.span(),
            "method receivers are not supported",
        ));
    };
    let Type::Path(context_path) = context.ty.as_ref() else {
        return Err(syn::Error::new(
            context.ty.span(),
            "expected typed QueryContext or MutationContext",
        ));
    };
    let context_segment = context_path.path.segments.last().expect("context path");
    let context_name = if mutation {
        "MutationContext"
    } else {
        "QueryContext"
    };
    if context_segment.ident != context_name {
        return Err(syn::Error::new(
            context.ty.span(),
            format!("expected {context_name}<'_, Identity>"),
        ));
    }
    let identity = match &context_segment.arguments {
        PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .find_map(|arg| match arg {
                GenericArgument::Type(ty) => Some(ty.clone()),
                _ => None,
            })
            .unwrap_or_else(|| syn::parse_quote!(())),
        _ => syn::parse_quote!(()),
    };
    let input = if args.len() == 2 {
        let FnArg::Typed(input) = args[1] else {
            return Err(syn::Error::new(
                item.sig.span(),
                "input must be an owned derived input",
            ));
        };
        (*input.ty).clone()
    } else {
        syn::parse_quote!(())
    };
    let has_input = args.len() == 2;
    let ReturnType::Type(_, output) = &item.sig.output else {
        return Err(syn::Error::new(
            item.sig.span(),
            "function must return Result<Output, hulladb::Error>",
        ));
    };
    let Type::Path(output) = output.as_ref() else {
        return Err(syn::Error::new(
            output.span(),
            "expected Result return type",
        ));
    };
    let output_segment = output.path.segments.last().expect("output path");
    if output_segment.ident != "Result" {
        return Err(syn::Error::new(
            output.span(),
            "function must return Result<Output, hulladb::Error>",
        ));
    }
    let PathArguments::AngleBracketed(output_args) = &output_segment.arguments else {
        return Err(syn::Error::new(
            output.span(),
            "expected Result<Output, Error>",
        ));
    };
    let Some(GenericArgument::Type(output)) = output_args.args.first() else {
        return Err(syn::Error::new(
            output.span(),
            "expected Result<Output, Error>",
        ));
    };
    let output = output.clone();
    let name = item.sig.ident.clone();
    let vis = item.vis.clone();
    let conditions: Vec<_> = item
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
        .cloned()
        .collect();
    let docs: Vec<_> = item
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .cloned()
        .collect();
    let input_alias = format_ident!("__hulladb_{name}_input");
    let output_alias = format_ident!("__hulladb_{name}_output");
    let identity_alias = format_ident!("__hulladb_{name}_identity");
    let handler = format_ident!("__hulladb_{name}_handler");
    item.sig.ident = handler.clone();
    item.vis = syn::Visibility::Inherited;
    let operation = operation.map_or_else(
        || quote!(concat!(module_path!(), "::", stringify!(#name))),
        |name| quote!(#name),
    );
    let context_type = if mutation {
        quote!(::hulladb::__private::managed::MutationContext)
    } else {
        quote!(::hulladb::__private::managed::QueryContext)
    };
    let handler_trait = if mutation {
        quote!(::hulladb::__private::managed::Mutation)
    } else {
        quote!(::hulladb::__private::managed::Query)
    };
    let register = if mutation {
        quote!(registry.mutation(NAME, Handler, ::hulladb::PolicyAdapter(authorization)))
    } else {
        quote!(registry.query(NAME, Handler, ::hulladb::PolicyAdapter(authorization)))
    };
    let register_with_concurrency = if mutation {
        quote!(
            pub fn register_with_concurrency(
                registry: &mut ::hulladb::Registry<super::#identity_alias>,
                authorization: impl ::hulladb::Authorization<super::#identity_alias>,
                concurrency: ::hulladb::MutationConcurrency,
            ) -> Result<(), ::hulladb::Error> {
                registry.mutation_with_concurrency(NAME, Handler, ::hulladb::PolicyAdapter(authorization), concurrency)
            }
        )
    } else {
        quote!()
    };
    let invoke = if has_input {
        quote!(super::#handler(context.into(), input))
    } else {
        quote!({ let _=input; super::#handler(context.into()) })
    };
    let parameters = if has_input {
        quote!(input: #input)
    } else {
        quote!()
    };
    let encode_input = if has_input {
        quote!(::hulladb::Codec::encode(&input)?)
    } else {
        quote!(::hulladb::__private::Json::Null)
    };
    let call = if mutation {
        quote!(::hulladb::__private::call_mutation)
    } else {
        quote!(::hulladb::__private::call_query)
    };
    let watch = if mutation {
        quote!(
            pub fn contract() -> ::hulladb::MutationContract {
                ::hulladb::MutationContract::new::<Input, Output>(NAME)
            }
        )
    } else {
        quote!(pub async fn watch(#parameters) -> Result<::hulladb::Subscription<Output>, ::hulladb::Error> { ::hulladb::__private::watch_query(NAME, #encode_input).await })
    };
    if !cfg!(feature = "server") {
        return Ok(quote! {
            #(#conditions)* #[allow(non_camel_case_types)] type #input_alias=#input;
            #(#conditions)* #[allow(non_camel_case_types)] type #output_alias=#output;
            #(#conditions)* #(#docs)*
            #vis async fn #name(#parameters) -> Result<#output,::hulladb::Error> { #call(#name::NAME, #encode_input).await }
            #(#conditions)* #vis mod #name {
                use super::*;
                pub const NAME:&str=#operation;
                pub type Input=super::#input_alias; pub type Output=super::#output_alias;
                #watch
            }
        });
    }
    Ok(quote! {
        #item
        #(#conditions)* #[allow(non_camel_case_types)] type #input_alias=#input;
        #(#conditions)* #[allow(non_camel_case_types)] type #output_alias=#output;
        #(#conditions)* #[allow(non_camel_case_types)] type #identity_alias=#identity;
        #(#conditions)* #(#docs)*
        #vis async fn #name(#parameters) -> Result<#output,::hulladb::Error> { #call(#name::NAME, #encode_input).await }
        #(#conditions)*
        #vis mod #name {
            use super::*;
            pub const NAME:&str=#operation;
            pub type Input=super::#input_alias; pub type Output=super::#output_alias;
            struct Handler;
            impl ::hulladb::__private::managed::FunctionContract for Handler {
                fn validate_input(&self,value:&::hulladb::__private::Json)->Result<(),::hulladb::Error> { <Input as ::hulladb::Codec>::decode(value).map(|_|()) }
                fn validate_result(&self,value:&::hulladb::__private::Json)->Result<(),::hulladb::Error> { <Output as ::hulladb::Codec>::decode(value).map(|_|()) }
            }
            impl #handler_trait<super::#identity_alias> for Handler {
                fn call<'a>(&'a self,context:#context_type<'a,super::#identity_alias>,value:&'a ::hulladb::__private::Json)->::hulladb::FunctionFuture<'a,::hulladb::__private::Json> {
                    Box::pin(async move { let input=<Input as ::hulladb::Codec>::decode(value)?; let output=::hulladb::__private::handler_scope(#invoke).await?; ::hulladb::Codec::encode(&output) })
                }
            }
            pub fn register(registry:&mut ::hulladb::Registry<super::#identity_alias>, authorization:impl ::hulladb::Authorization<super::#identity_alias>)->Result<(),::hulladb::Error> { #register }
            #register_with_concurrency
            #watch
        }
    })
}
