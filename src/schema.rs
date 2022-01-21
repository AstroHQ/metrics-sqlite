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
joinable!(metrics -> metric_keys (metric_key_id));
allow_tables_to_appear_in_same_query!(metrics, metric_keys);
// allow_tables_to_appear_in_same_query!(counters,);
