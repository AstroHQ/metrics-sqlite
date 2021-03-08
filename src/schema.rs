table! {
    metrics (id) {
        id -> BigInt,
        timestamp -> Double,
        key -> Text,
        value -> Double,
    }
}

// allow_tables_to_appear_in_same_query!(counters,);
