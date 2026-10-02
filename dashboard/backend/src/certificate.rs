use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

use acmex::{AcmeConfig, ChallengeSolverRegistry, Contact, Dns01Solver, DnsProvider};
use shared::{
    database::{
        certificate::DatabaseCertificateModifiyRepository,
        dnsprovider::DatabaseDNSProviderRepository, get_database,
    },
    models::certificate::{NeedSignCertificate, UpdateCertificate},
    objectid::ObjectId,
};
use tokio::{sync::RwLock, task::JoinHandle};
use tokio_schedule::Job;
use tracing::{Level, event};

/// 进程内正在签发的任务表。
///
/// 注意：它**只是**本进程内的视图，跨进程互斥由数据库的
/// `certificates.signing_started_at` + `FOR UPDATE SKIP LOCKED` 保证（ISSUES.md P0-14）。
pub static PENDINGS: LazyLock<RwLock<HashMap<ObjectId, JoinHandle<()>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// 单轮最多认领多少张证书，避免一次拉取把 ACME 配额在短时间内打满。
const MAX_CONCURRENT_SIGNINGS: usize = 4;

/// 调度器任务句柄。`init()` 内部用 `tokio::spawn` 起了 `tokio_schedule` 任务后立即返回，
/// 原先 `main.rs` 保存的 JoinHandle 指向的是**已经结束的** `init()` 任务，
/// 关闭时的 `auto_cert.abort()` 无法停止调度器（ISSUES.md P0-14 问题 C）。
/// 这里把真正的调度器句柄暴露出来。
pub async fn init() -> anyhow::Result<JoinHandle<()>> {
    let handle = tokio::spawn(tokio_schedule::every(30).seconds().perform(|| async {
        if let Err(e) = fetch_new_certificates().await {
            event!(Level::ERROR, "Failed to fetch new certificates: {e}");
        }
    }));
    Ok(handle)
}

pub async fn fetch_new_certificates() -> anyhow::Result<()> {
    for _ in 0..MAX_CONCURRENT_SIGNINGS {
        // 抢占式认领：只有拿到行锁的实例才会得到 `Some`。
        let certificate = match get_database().try_claim_certificate_signing().await? {
            Some(cert) => cert,
            // 没有待签发的证书了。
            None => break,
        };
        let id = certificate.id;
        event!(Level::INFO, "Claimed certificate for signing: {id}");

        let handle = tokio::spawn(async move {
            // `SigningGuard` 保证无论正常返回、提前 return 还是 panic，
            // 都会释放数据库侧的认领标记（ISSUES.md P0-14 问题 B）。
            let _guard = SigningGuard { id };
            sign(certificate).await;
        });
        PENDINGS.write().await.insert(id, handle);
    }
    Ok(())
}

/// 签发认领的 Drop 守卫。
///
/// 释放动作在 `Drop` 里执行，因此 panic 展开（acmex 内部 `unwrap`/`expect`）时
/// 同样会释放。原实现在 `sign()` 末尾手写 `pendings.remove(&id)`，
/// panic 路径不会执行 → 该证书永久停留在"已认领"状态，再也不被重试，静默到期。
struct SigningGuard {
    id: ObjectId,
}

impl Drop for SigningGuard {
    fn drop(&mut self) {
        let id = self.id;
        // Drop 中不能 await，因此把清理动作 spawn 出去，并让它自己拿到一个连接。
        tokio::spawn(async move {
            match get_database().release_certificate_signing(&id).await {
                Ok(()) => event!(Level::DEBUG, "Released signing claim for {id}"),
                Err(e) => event!(
                    Level::ERROR,
                    "Failed to release signing claim for {id}: {e}. \
                     This certificate will not be retried until the claim is cleared manually"
                ),
            }
            PENDINGS.write().await.remove(&id);
        });
    }
}

async fn inner_sign(cert: NeedSignCertificate) -> anyhow::Result<()> {
    let dns = get_database()
        .get_dns_provider_by_id(&cert.dns_provider_id)
        .await?;
    let mut client = acmex::AcmeClient::new(
        AcmeConfig::new("https://acme.zerossl.com/v2/DV90")
            .with_contact(Contact::email(cert.email))
            .with_tos_agreed(true),
    )?;
    let dns_provider: Arc<dyn DnsProvider> = Arc::new(match dns.provider {
        shared::models::dnsprovider::DatabaseDNSProviderKind::TENCENT(tencent) => {
            acmex::dns::providers::TencentCloudDnsProvider::new(
                tencent.secret_id,
                tencent.secret_key,
                "".to_string(),
            )
        }
    });

    let mut solver_registry = ChallengeSolverRegistry::new();
    for domain in dns.domains {
        solver_registry.register(Dns01Solver::new(dns_provider.clone(), domain));
    }

    let bundle = client
        .issue_certificate(cert.hostnames, &mut solver_registry)
        .await?;
    let fullchain = bundle.certificate_pem;
    let key = bundle.private_key_pem;
    let final_cert = UpdateCertificate::new(cert.id, fullchain, key);
    get_database().update_certificate(&final_cert).await?;

    Ok(())
}

pub async fn sign(cert: NeedSignCertificate) {
    let id = cert.id;
    if let Err(e) = inner_sign(cert).await {
        event!(Level::ERROR, "Failed to sign [{id}] certificate: {e}");
    } else {
        event!(Level::INFO, "Finish to sign certificate: {id}");
    }
    // 认领标记由 `SigningGuard` 的 Drop 释放，正常与 panic 路径都覆盖。
}
