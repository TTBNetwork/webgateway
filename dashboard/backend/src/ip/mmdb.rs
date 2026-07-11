use geoip2::{City, Country, Reader};
use std::{net::IpAddr, path::PathBuf, sync::LazyLock};

use crate::ip::{LookupResult, ROOT};

// Reader now holds a 'static reference to the mmdb data
pub struct CityInstance {
    // We keep the slice only to conceptually own the data,
    // but it's not strictly necessary for functionality.
    _data: &'static [u8],
    reader: Reader<'static, City<'static>>,
}

impl CityInstance {
    pub fn new(data: &'static [u8]) -> Self {
        Self {
            _data: data,
            reader: Reader::<City>::from_bytes(data).unwrap(),
        }
    }

    pub fn lookup(&self, ip: IpAddr) -> anyhow::Result<LookupResult> {
        let result = self
            .reader
            .lookup(ip)
            .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
        let iso_code = result
            .country
            .and_then(|v| v.iso_code.map(|v| v.to_string()));
        let city = result.subdivisions.and_then(|v| {
            // v.and_then(|v| v.iso_code) find iso_code is not null else return None
            v.iter().find_map(|v| v.iso_code.map(|v| v.to_string()))
        });
        Ok(LookupResult {
            ip,
            country: iso_code,
            city,
        })
    }
}

pub struct CountryInstance {
    // We keep the slice only to conceptually own the data,
    // but it's not strictly necessary for functionality.
    _data: &'static [u8],
    reader: Reader<'static, Country<'static>>,
}

impl CountryInstance {
    pub fn new(data: &'static [u8]) -> Self {
        Self {
            _data: data,
            reader: Reader::<Country>::from_bytes(data).unwrap(),
        }
    }

    pub fn lookup(&self, ip: IpAddr) -> anyhow::Result<LookupResult> {
        let result = self
            .reader
            .lookup(ip)
            .map_err(|e| anyhow::anyhow!(format!("{e:?}")))?;
        // println!("Country: {ip:?} {:?}", result);
        let iso_code = result
            .country
            .and_then(|v| v.iso_code.map(|v| v.to_string()));
        let city = result.continent.and_then(|v| v.code.map(|v| v.to_string()));
        Ok(LookupResult {
            ip,
            country: iso_code,
            city,
        })
    }
}

static CITY_FILE: LazyLock<PathBuf> = LazyLock::new(|| ROOT.clone().join("GeoLite2-City.mmdb"));
static CHINA_COUNTRY_FILE: LazyLock<PathBuf> =
    LazyLock::new(|| ROOT.clone().join("China_Country.mmdb"));

static CITY_INSTANCE: LazyLock<CityInstance> = LazyLock::new(|| {
    let content = std::fs::read(CITY_FILE.clone()).unwrap();
    // Leak the Vec to obtain a 'static slice
    let leaked: &'static [u8] = Box::leak(Box::new(content));
    CityInstance::new(leaked)
});

static CHINA_COUNTRY_INSTANCE: LazyLock<CountryInstance> = LazyLock::new(|| {
    let content = std::fs::read(CHINA_COUNTRY_FILE.clone()).unwrap();
    // Leak the Vec to obtain a 'static slice
    let leaked: &'static [u8] = Box::leak(Box::new(content));
    CountryInstance::new(leaked)
});

pub fn lookup(ip: IpAddr) -> anyhow::Result<LookupResult> {
    match CHINA_COUNTRY_INSTANCE.lookup(ip) {
        Ok(v) => Ok(v),
        Err(_) => CITY_INSTANCE.lookup(ip),
    }
}
