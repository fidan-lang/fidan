//! Export existing compiler-owned metadata for website drift checks. No new CLI API.
use fidan_config::{
    LANGUAGE_BUILTINS, LANGUAGE_DECORATORS, LANGUAGE_TYPE_NAMES, ReceiverBuiltinKind as Kind,
    receiver_member_signature, receiver_member_specs,
};
use fidan_stdlib::{STDLIB_MODULES, module_members};
use serde_json::json;

fn main() {
    let modules = STDLIB_MODULES.iter().map(|module| {
        let members = module_members(module.name).iter().map(|member| json!({
            "names": member.names, "signature": fidan_stdlib::member_signature(module.name, member.names[0]), "doc": member.doc,
            "return_type": fidan_stdlib::member_return_type(module.name, member.names[0]),
            "arity": fidan_config::stdlib::callable_arity(module.name, member.names[0]),
        })).collect::<Vec<_>>();
        json!({"name": module.name, "doc": module.doc, "exports": (module.exports)(), "members": members})
    }).collect::<Vec<_>>();
    // Enum iteration only: member names, signatures and aliases still come from
    // fidan-config. Include empty families so future members cannot disappear.
    let receivers = [
        (Kind::Integer, "integer"),
        (Kind::Float, "float"),
        (Kind::Boolean, "boolean"),
        (Kind::String, "string"),
        (Kind::List, "list"),
        (Kind::Dict, "dict"),
        (Kind::HashSet, "hashset"),
        (Kind::Handle, "handle"),
        (Kind::Nothing, "nothing"),
        (Kind::Dynamic, "dynamic"),
        (Kind::Shared, "Shared"),
        (Kind::WeakShared, "WeakShared"),
        (Kind::Pending, "Pending"),
        (Kind::Function, "action"),
    ]
    .into_iter()
    .map(|(kind, name)| {
        let members = receiver_member_specs(kind)
            .iter()
            .map(|member| {
                json!({
                    "names": member.names,
                    "signature": receiver_member_signature(kind, member.info.canonical_name),
                    "has_field": member.info.field_return.is_some(),
                    "has_method": member.info.method_return.is_some(),
                })
            })
            .collect::<Vec<_>>();
        json!({"name": name, "members": members})
    })
    .collect::<Vec<_>>();
    let builtins = LANGUAGE_BUILTINS
        .iter()
        .map(|v| json!({"name":v.name,"signature":v.signature,"doc":v.doc}))
        .collect::<Vec<_>>();
    let types = LANGUAGE_TYPE_NAMES
        .iter()
        .map(|v| json!({"name":v.name,"signature":v.signature,"doc":v.doc}))
        .collect::<Vec<_>>();
    let decorators = LANGUAGE_DECORATORS
        .iter()
        .map(|v| json!({"name":v.name,"doc":v.doc,"reserved":v.reserved_only}))
        .collect::<Vec<_>>();
    println!("{}", serde_json::to_string_pretty(&json!({"modules":modules,"builtins":builtins,"types":types,"decorators":decorators,"receivers":receivers})).unwrap());
}
