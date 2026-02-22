use serde_json::Value;
use std::sync::OnceLock;

static EN_STRINGS: &str = include_str!("../resources/i18n/en.json");
static STRINGS: OnceLock<Value> = OnceLock::new();

pub fn locale() -> &'static str {
    "en"
}

pub fn strings() -> &'static Value {
    STRINGS.get_or_init(|| serde_json::from_str(EN_STRINGS).unwrap_or(Value::Null))
}

#[cfg(feature = "launcher")]
pub fn text(key: &str) -> String {
    lookup(strings(), key)
        .and_then(Value::as_str)
        .map(std::borrow::ToOwned::to_owned)
        .unwrap_or_else(|| key.to_owned())
}

#[cfg(feature = "launcher")]
fn lookup<'a>(root: &'a Value, dotted_key: &str) -> Option<&'a Value> {
    let mut node = root;
    for segment in dotted_key.split('.') {
        node = node.get(segment)?;
    }
    Some(node)
}
