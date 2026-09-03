use serde::Deserialize;

/// One aircraft record from an ADS-B Exchange JSON snapshot.
#[derive(Debug, Clone, Deserialize)]
pub struct Aircraft {
    pub hex: String,
    #[serde(rename = "flight")]
    pub callsign: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub alt_baro: Option<serde_json::Value>,
    pub gs: Option<f64>,
    pub track: Option<f64>,
    pub baro_rate: Option<f64>,
}

impl Aircraft {
    pub fn alt_baro_feet(&self) -> Option<f64> {
        match &self.alt_baro {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            _ => None,
        }
    }
}

/// Top-level ADS-B Exchange JSON snapshot.
#[derive(Debug, Clone, Deserialize)]
pub struct AdsbSnapshot {
    pub now: f64,
    pub aircraft: Vec<Aircraft>,
}

impl AdsbSnapshot {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

/// Returns true if the aircraft position is within `radius_km` of the center.
/// Pass `None` radius to accept all.
pub fn in_geo_filter(
    lat: f64,
    lon: f64,
    center_lat: Option<f64>,
    center_lon: Option<f64>,
    radius_km: Option<f64>,
) -> bool {
    let (Some(clat), Some(clon), Some(radius)) = (center_lat, center_lon, radius_km) else {
        return true;
    };
    haversine_km(lat, lon, clat, clon) <= radius
}

/// Return the haversine distance between two points.
///
/// The haversine distance is the point-to-point great circle distance (taking into
/// account the curvature of the Earth). It ignores elevation and assumes
/// the Earth is a sphere but these assumptions yield very little error
/// over shorter distances (typically 0.1%-0.3% and not larger than 0.5%) which
/// is acceptable for this application
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_haversine_zero() {
        assert!((haversine_km(40.0, -75.0, 40.0, -75.0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_in_geo_filter_no_filter() {
        assert!(in_geo_filter(0.0, 0.0, None, None, None));
    }

    #[test]
    fn test_in_geo_filter_inside() {
        assert!(in_geo_filter(
            40.0,
            -75.0,
            Some(40.0),
            Some(-75.0),
            Some(100.0)
        ));
    }

    #[test]
    fn test_in_geo_filter_outside() {
        assert!(!in_geo_filter(
            50.0,
            -75.0,
            Some(40.0),
            Some(-75.0),
            Some(100.0)
        ));
    }

    #[test]
    fn test_parse_snapshot() {
        let json = r#"{"now":1700000000.0,"aircraft":[{"hex":"abc123","lat":40.0,"lon":-75.0,"alt_baro":35000}]}"#;
        let snap = AdsbSnapshot::from_json(json).unwrap();
        assert_eq!(snap.aircraft.len(), 1);
        assert_eq!(snap.aircraft[0].hex, "abc123");
        assert_eq!(snap.aircraft[0].alt_baro_feet(), Some(35000.0));
    }

    #[test]
    fn test_parse_snapshot_alt_ground() {
        let json = r#"{"now":1700000000.0,"aircraft":[{"hex":"abc","alt_baro":"ground"}]}"#;
        let snap = AdsbSnapshot::from_json(json).unwrap();
        assert!(snap.aircraft[0].alt_baro_feet().is_none());
    }
}
