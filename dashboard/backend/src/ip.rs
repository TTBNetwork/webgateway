use std::{net::IpAddr, path::PathBuf, sync::LazyLock};

use serde::{Deserialize, Serialize};

pub mod cz88;
pub mod mmdb;
// pub mod ip2region;

static ROOT: LazyLock<PathBuf> = LazyLock::new(|| PathBuf::from("./").join("assets").join("ipdb"));

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LookupResult {
    pub ip: IpAddr,
    pub country: Option<String>,
    pub city: Option<String>,
}

pub fn lookup(ip: IpAddr) -> anyhow::Result<LookupResult> {
    // let ip2region_result = ip2region::lookup(ip)?;
    // if let Some(country) = &ip2region_result.country && country == "CN" && ip2region_result.city.is_some() {
    //     return Ok(ip2region_result);
    // }
    match mmdb::lookup(ip) {
        Ok(r) => match &r.country {
            Some(country) => {
                if country == "CN" {
                    cz88::lookup(ip)
                } else {
                    Ok(r)
                }
            }
            None => cz88::lookup(ip),
        },
        Err(e) => Err(e),
    }
}
