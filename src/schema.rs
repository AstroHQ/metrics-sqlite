table! {
    metrics (id) {
        id -> BigInt,
        timestamp -> Double,
        metric_key_id -> BigInt,
        value -> Double,
    }
}
table! {
    metric_keys (id) {
        id -> BigInt,
        key -> Text,
        unit -> Text,
        description -> Text,
    }
}

// allow_tables_to_appear_in_same_query!(counters,);
