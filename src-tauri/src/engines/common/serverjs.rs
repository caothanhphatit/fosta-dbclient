//! MongoDB operators that run JavaScript on the server. The grid filter check (gateway) and the MongoDB engine operations
//! both refuse them, so the list lives here, in the toolkit both may use.

/// MongoDB operators that run JavaScript on the server.
pub const DENIED_MONGO_OPERATORS: [&str; 3] = ["$where", "$function", "$accumulator"];

/// The first operator in a JSON value that runs JavaScript on the server, at any depth.
pub fn find_denied_operator(value: &serde_json::Value) -> Option<&'static str> {
    match value {
        serde_json::Value::Object(map) => map.iter().find_map(|(key, inner)| {
            DENIED_MONGO_OPERATORS
                .iter()
                .find(|operator| **operator == key)
                .copied()
                .or_else(|| find_denied_operator(inner))
        }),
        serde_json::Value::Array(items) => items.iter().find_map(find_denied_operator),
        _ => None,
    }
}
