// ABRAXAS config: ISO 6709 parsing, the zone table, the localtime link,
// load errors, and the TOML round trip with the legacy ini migration.

use std::fs;

use abraxas::config::*;

fn scratch(name: &str) -> Paths {
    let dir = std::env::temp_dir()
        .join(format!("abraxas-config-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    Paths::in_dir(&dir)
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn iso6709_minutes_and_seconds() {
    let la = parse_iso6709("+340308-1181434").unwrap();
    assert!(close(la.lat, 34.0 + 3.0 / 60.0 + 8.0 / 3600.0));
    assert!(close(la.lon, -(118.0 + 14.0 / 60.0 + 34.0 / 3600.0)));

    let paris = parse_iso6709("+4852+00220").unwrap();
    assert!(close(paris.lat, 48.0 + 52.0 / 60.0));
    assert!(close(paris.lon, 2.0 + 20.0 / 60.0));

    let london = parse_iso6709("+513030-0000731").unwrap();
    assert!(london.lon < 0.0 && london.lon > -0.2);
}

#[test]
fn iso6709_rejects_malformed() {
    for s in ["", "+", "+3403", "3403-11814", "+34x3-11814", "+340-11814", "+9900+00000"] {
        assert!(parse_iso6709(s).is_none(), "{s:?} parsed");
    }
}

#[test]
fn zone_table_lookup() {
    let table = "# comment\tline\n\
                 FR,MC\t+4852+00220\tEurope/Paris\n\
                 US\t+340308-1181434\tAmerica/Los_Angeles\tPacific\n";
    let la = zone_coordinates(table, "America/Los_Angeles").unwrap();
    assert!(close(la.lat, 34.0522));
    assert!(zone_coordinates(table, "Europe/Paris").is_some());
    assert!(zone_coordinates(table, "America/Chicago").is_none());
    assert!(zone_coordinates(table, "Europe").is_none());
}

#[test]
fn zone_from_localtime_link() {
    assert_eq!(
        zone_from_path("/usr/share/zoneinfo/America/Los_Angeles").as_deref(),
        Some("America/Los_Angeles")
    );
    assert_eq!(zone_from_path("../usr/share/zoneinfo/UTC").as_deref(), Some("UTC"));
    assert!(zone_from_path("/usr/share/zoneinfo/").is_none());
    assert!(zone_from_path("/etc/localtime").is_none());
}

#[test]
fn load_reports_missing_and_invalid() {
    let p = scratch("missing");
    assert!(matches!(load_location(&p), Err(ConfigError::Missing)));

    fs::write(&p.config_file, "[location]\nlatitude = 91.0\nlongitude = 0.0\n").unwrap();
    assert!(matches!(load_location(&p), Err(ConfigError::Invalid(_))));

    fs::write(&p.config_file, "[location]\nlatitude = \"north\"\n").unwrap();
    assert!(matches!(load_location(&p), Err(ConfigError::Invalid(_))));
}

#[test]
fn invalid_config_is_never_overwritten() {
    let p = scratch("invalid");
    let body = "[location\nlatitude = 1\n";
    fs::write(&p.config_file, body).unwrap();
    assert!(matches!(load_or_create_location(&p), Err(ConfigError::Invalid(_))));
    assert_eq!(fs::read_to_string(&p.config_file).unwrap(), body);
}

#[test]
fn save_then_load_round_trips() {
    let p = scratch("roundtrip");
    save_location(&p, 41.88, -87.63).unwrap();
    let loc = load_location(&p).unwrap();
    assert!(close(loc.lat, 41.88) && close(loc.lon, -87.63));
    assert!(save_location(&p, 0.0, 200.0).is_err());
}

#[test]
fn legacy_ini_migrates_once() {
    let p = scratch("migrate");
    fs::write(&p.legacy_ini, "[location]\nlatitude = 41.880000\nlongitude = -87.630000\n")
        .unwrap();
    let loc = load_or_create_location(&p).unwrap();
    assert!(close(loc.lat, 41.88) && close(loc.lon, -87.63));
    assert!(!p.legacy_ini.exists());
    let again = load_location(&p).unwrap();
    assert_eq!(loc, again);
}
