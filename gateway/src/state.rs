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
            // P0-6：这里原先从未调用 `.max_connections(..)`，于是 `max_connections`
            // 保持 0，被 `UpstreamConnectionPool::new` 当成"无上限"
            // （`Semaphore::MAX_PERMITS`），每个并发请求各占一条上游连接，
            // fd 与上游连接数随并发线性增长。
            //
            // 现在是有限上限：默认 `DEFAULT_MAX_CONNECTIONS`，也可通过
            // `UpstreamConnectionPoolConfig::max_connections()` 覆盖。
            //
            // 已知残留（诚实记录）：由于上游连接由 hyper 的 `Connection` future
            // 持有（最长 120s keep-alive），稳态下并发请求仍会各自占用一个许可，
            // 池中的**空闲复用率依然很低**。这次改动解决的是"无上限"和
            // "许可被提前释放导致上限形同虚设"，并没有把连接池变成真正的
            // keep-alive 复用池 —— 后者需要在响应体读完后主动把连接交还给池，
            // 属于后续优化。副作用是：并发超过上限时请求会排队，
            // 超过 `ACQUIRE_PERMIT_TIMEOUT` 快速失败（502）而不是无限挂起。
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
