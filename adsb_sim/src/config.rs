use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AdsbSimConfig {
    pub json_file: Option<String>,
    pub datetime_start: Option<chrono::DateTime<chrono::Utc>>,
    pub datetime_end: Option<chrono::DateTime<chrono::Utc>>,
    pub geo_center_lat: Option<f64>,
    pub geo_center_lon: Option<f64>,
    pub geo_radius_km: Option<f64>,
    pub speed_multiplier: f64,
    pub delete_timeout_secs: f64,
}

impl Default for AdsbSimConfig {
    fn default() -> Self {
        Self {
            json_file: None,
            datetime_start: None,
            datetime_end: None,
            geo_center_lat: None,
            geo_center_lon: None,
            geo_radius_km: None,
            speed_multiplier: 1.0,
            delete_timeout_secs: 30.0,
        }
    }
}
