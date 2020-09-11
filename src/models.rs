use crate::schema::metrics;

#[derive(Insertable, Debug)]
#[table_name = "metrics"]
pub struct NewMetric {
    pub timestamp: f64,
    pub key: String,
    pub value: i64,
}
