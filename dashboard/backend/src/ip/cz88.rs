use std::{collections::HashMap, net::IpAddr, path::PathBuf, sync::LazyLock};

use anyhow::Result;
use ipdb::Reader;

use crate::ip::{LookupResult, ROOT};

static FILE: LazyLock<PathBuf> = LazyLock::new(|| ROOT.clone().join("qqwry.ipdb"));

pub struct Instance {
    instance: Reader,
}

static COUNTRY_CODE_MAPPINGS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    let content = include_str!("../../../../assets/ipdb/country_code_mappings");
    for line in content.lines() {
        let mut parts = line.split("|");
        let country = parts.next().unwrap();
        let code = parts.next().unwrap();
        m.insert(country.to_string(), code.to_string());
    }
    m
});

static CITY_CODE_MAPPINGS: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    let content = include_str!("../../../../assets/ipdb/city_code_mappings");
    for line in content.lines() {
        let mut parts = line.split("|");
        let city = parts.next().unwrap();
        let code = parts.next().unwrap();
        m.insert(city.to_string(), code.to_string());
    }
    m
});

impl Instance {
    pub fn new(instance: Reader) -> Self {
        Self { instance }
    }

    fn lookup(&self, ip: IpAddr) -> Result<LookupResult> {
        let res = self.instance.find_city_info(&ip.to_string(), "CN")?;
        Ok(LookupResult {
            ip,
            country: if !res.country_name.trim().is_empty() {
                Some(res.country_name.to_string())
            } else {
                None
            },
            city: if !res.region_name.trim().is_empty() {
                Some(res.region_name.to_string())
            } else {
                None
            },
        })
    }
}

static INSTANCE: LazyLock<Instance> =
    LazyLock::new(|| Instance::new(ipdb::Reader::open_file(&*FILE).unwrap()));

pub fn lookup(ip: IpAddr) -> Result<LookupResult> {
    INSTANCE.lookup(ip).map(|mut v| {
        v.country = v.country.as_ref().map(|country| {
            COUNTRY_CODE_MAPPINGS
                .get(country)
                .unwrap_or(country)
                .to_string()
        });
        v.city = v
            .city
            .as_ref()
            .map(|city| CITY_CODE_MAPPINGS.get(city).unwrap_or(city).to_string());
        v
    })
}
