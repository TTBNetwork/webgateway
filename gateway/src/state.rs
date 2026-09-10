use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use anyhow::anyhow;
use protocols::tls::ProtocolTLS;
use shared::{models::websites::DatabaseWebsite, objectid::ObjectId};
use tokio::net::lookup_host;

// 注意：模块路径可能需根据实际项目调整，这里假设已重命名为 upstreams
use crate::upstream::connection::{UpstreamConnectionPool, UpstreamConnectionPoolConfig};

#[derive(Debug)]
pub struct WebSiteRunner {
    inner: DatabaseWebsite,
    pool: Arc<UpstreamConnectionPool>, // 类型替换
}

impl WebSiteRunner {
    pub async fn new(inner: DatabaseWebsite) -> anyhow::Result<Self> {
        // 目前只使用第一个 backend（保留 TODO）
        let backend = inner
            .backends
            .first()
            .ok_or(anyhow!("No found any backends"))?;
        let hostname = backend.url.host_str().ok_or(anyhow!("No found any host"))?;
        // DNS 解析
        let addrs = lookup_host(format!(
            "{hostname}:{}",
            backend.url.port_or_known_default().unwrap_or(80)
        ))
        .await?
        .collect::<Vec<SocketAddr>>();
        let url = backend.url.clone();

        // 若解析结果为空，可提前返回错误
        if addrs.is_empty() {
            return Err(anyhow!("No IP addresses resolved for {}", hostname));
        }

        Ok(Self {
            inner,
            pool: UpstreamConnectionPool::new(
                UpstreamConnectionPoolConfig::new_from_targets(addrs).url(url),
            ),
        })
    }

    pub fn inner(&self) -> &DatabaseWebsite {
        &self.inner
    }

    pub fn pool(&self) -> &Arc<UpstreamConnectionPool> {
        &self.pool
    }
}

#[derive(Debug, Clone)]
pub struct BaseClientState {
    pub tls: Option<ProtocolTLS>,
    pub remote_addr: IpAddr,
    pub local_addr: IpAddr,
}

#[derive(Debug, Clone)]
pub struct ClientState {
    pub base: Arc<BaseClientState>,
    pub website: Arc<WebSiteRunner>,
    pub host: String,
    pub id: ObjectId,
}

impl ClientState {
    pub fn new(
        base: Arc<BaseClientState>,
        website: Arc<WebSiteRunner>,
        host: String,
        id: &ObjectId,
    ) -> Self {
        Self {
            base,
            website,
            host,
            id: *id,
        }
    }

    pub fn tls(&self) -> Option<&ProtocolTLS> {
        self.base.tls.as_ref()
    }
    pub fn remote_addr(&self) -> IpAddr {
        self.base.remote_addr
    }
    pub fn local_addr(&self) -> IpAddr {
        self.base.local_addr
    }
    pub fn scheme(&self) -> &str {
        if self.tls().is_some() {
            "https"
        } else {
            "http"
        }
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn id(&self) -> &ObjectId {
        &self.id
    }
}
