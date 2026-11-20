//! Optional MaxMind GeoLite2 lookup for the dashboard access map.
//!
//! The database is intentionally not bundled with the repository (MaxMind
//! licence). Set `GEOIP_MMDB` to a GeoLite2-City.mmdb file on the host. When
//! the file is absent the API reports `configured: false` and the UI explains
//! how to enable it instead of failing requests.

use std::{net::IpAddr, path::PathBuf, sync::OnceLock};

use geoip2::{City, Reader};

pub struct GeoIp {
    reader: Reader<'static, City<'static>>,
    // Keeps the leaked database bytes conceptually owned by this instance.
    _data: &'static [u8],
}

static INSTANCE: OnceLock<Option<GeoIp>> = OnceLock::new();

fn database_path() -> Option<PathBuf> {
    std::env::var_os("GEOIP_MMDB")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn load() -> Option<GeoIp> {
    let path = database_path()?;
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = ?error, "GEOIP_MMDB could not be read");
            return None;
        }
    };
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    let reader = match Reader::<City>::from_bytes(leaked) {
        Ok(reader) => reader,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = ?error, "GEOIP_MMDB is not a valid MaxMind database");
            return None;
        }
    };
    tracing::info!(path = %path.display(), "GeoIP database loaded");
    Some(GeoIp {
        reader,
        _data: leaked,
    })
}

fn instance() -> Option<&'static GeoIp> {
    INSTANCE.get_or_init(load).as_ref()
}

pub fn configured() -> bool {
    instance().is_some()
}

/// Returns the ISO 3166-1 alpha-2 country code for a public address.
pub fn lookup_country(ip: IpAddr) -> Option<String> {
    let geo = instance()?;
    let result = geo.reader.lookup(ip).ok()?;
    result
        .country
        .and_then(|country| country.iso_code)
        .map(|code| code.to_string())
}
