table! {
    metrics (id) {
        id -> BigInt,
        timestamp -> Double,
        key -> Text,
        value -> BigInt,
    }
}

// allow_tables_to_appear_in_same_query!(counters,);
