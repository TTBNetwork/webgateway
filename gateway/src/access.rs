use std::{
    sync::{
        Arc, LazyLock, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, TimeDelta, Timelike, Utc};
use dashmap::DashMap;
use http_body::SizeHint;
use hyper::{HeaderMap, Uri, Version};
use shared::{
    database::{access::DatabaseAccessLogsModifyRepository, get_database},
    models::access::{
        AccessCreateRequest, AccessCreateResponse, AccessInsertRequestSize,
        AccessInsertResponseSize, AccessUpdateRequestSize, AccessUpdateResponseSize, AccessVersion,
    },
    objectid::ObjectId,
};
use tracing::{Level, event};

/// 单条语句一次最多写入的行数，与 `crates/shared/src/database/access.rs` 的分块保持一致。
/// 这里再兜一层，避免单轮刷盘把整秒的流量塞进一条超大语句。
const MAX_ROWS_PER_FLUSH: usize = 1_000;
/// 刷盘循环的最小间隔。原实现用 `.max(100µs)` 作为"最少睡多久"的下限，
/// 在 DB 变慢（sync 超过 1s）时会把间隔钳到 100µs，形成"越慢越快"的正反馈（ISSUES.md P0-4）。
const MIN_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// 单轮 sync 超过该阈值即认为 DB 已经跟不上，记录告警。
const FLUSH_LAG_WARN: Duration = Duration::from_secs(3);
/// 内存中待落库条目超过该数量即告警。为了不丢日志，刷盘失败时数据会保留在内存里，
/// 因此 DB 长时间不可用会导致内存增长 —— 必须让运维看到这个积压。
const FLUSH_BACKLOG_WARN: usize = 200_000;

#[derive(Debug, Clone)]
pub struct RequestLog {
    pub inner: AccessCreateRequest,
}

#[derive(Debug, Clone)]
pub struct RequestContext {
    pub req_id: ObjectId,
    pub host: String,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub method: hyper::Method,
    pub version: Version,
    pub body_length: SizeHint,
    pub remote_addr: String,
    pub website_id: Option<ObjectId>,
}

impl RequestLog {
    pub fn new(context: RequestContext) -> anyhow::Result<Self> {
        Ok(Self {
            inner: AccessCreateRequest {
                id: context.req_id,
                method: context.method.to_string(),
                path: match context.uri.path_and_query() {
                    Some(v) => v.to_string(),
                    None => "".to_string(),
                },
                headers: {
                    let mut converted_headers = vec![];
                    for (k, v) in context.headers.iter() {
                        converted_headers
                            .push((k.to_string(), v.to_str().unwrap_or_default().to_string()));
                    }
                    converted_headers
                },
                host: context.host,
                http_version: {
                    match context.version {
                        Version::HTTP_09 => AccessVersion::HTTP09,
                        Version::HTTP_10 => AccessVersion::HTTP10,
                        Version::HTTP_11 => AccessVersion::HTTP11,
                        Version::HTTP_2 => AccessVersion::HTTP2,
                        Version::HTTP_3 => AccessVersion::HTTP3,
                        version => return Err(anyhow::anyhow!("Unknown version: {version:?}")),
                    }
                },
                remote_addr: context.remote_addr,
                body_length: context.body_length.lower().min(u64::from(u32::MAX)) as usize,
                requested_at: get_database().get_database_time()?,
                website_id: context.website_id,
            },
        })
    }
}

#[derive(Debug, Clone)]
pub struct ResponseLog {
    pub inner: AccessCreateResponse,
}
impl ResponseLog {
    pub fn new(
        id: ObjectId,
        http_version: Version,
        headers: &HeaderMap,
        status: u16,
        body_length: SizeHint,
        backend_responsed_at: Option<DateTime<Utc>>,
        website_id: Option<ObjectId>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: AccessCreateResponse {
                id,
                status,
                headers: {
                    let mut converted_headers = vec![];
                    for (k, v) in headers.iter() {
                        converted_headers
                            .push((k.to_string(), v.to_str().unwrap_or_default().to_string()));
                    }
                    converted_headers
                },
                http_version: {
                    match http_version {
                        Version::HTTP_09 => AccessVersion::HTTP09,
                        Version::HTTP_10 => AccessVersion::HTTP10,
                        Version::HTTP_11 => AccessVersion::HTTP11,
                        Version::HTTP_2 => AccessVersion::HTTP2,
                        Version::HTTP_3 => AccessVersion::HTTP3,
                        version => return Err(anyhow::anyhow!("Unknown version: {version:?}")),
                    }
                },
                body_length: body_length.lower().min(u64::from(u32::MAX)) as usize,
                responsed_at: get_database().get_database_time()?,
                backend_responsed_at,
                website_id,
            },
        })
    }
}

// ==================== 待落库缓冲区 ====================
//
// 设计要点（ISSUES.md P0-1）：**先写库成功，再从内存移除**。
// 旧实现是「先从内存删除 → 再写库」，写库失败（连接池超时、PG 重启、参数超限）
// 时数据已经从内存消失，只能永久丢失。
//
// 现在每一类缓冲区都有两个槽位：
//   * `pending`  —— 生产者（请求处理路径）只做 append，永不删除；
//   * `inflight` —— 刷盘器取走准备写库的批次，写库成功后才清空，失败则合并回 `pending`。
//
// 合并回 `pending` 时按「先到先得」处理重复 key：若该 key 在刷盘期间已被写入更新的值，
// 保留更新的那份，避免用旧值覆盖新值。

/// 按时间窗口切分的待落库缓冲区（请求 / 响应 / size 增量日志）。
///
/// 生产者只做 append，永不删除；刷盘器按 `key < cutoff` 取出已到齐的条目。
/// `inflight` 保存"已取出、正在写库"的批次：写库成功才清空，失败则合并回队首重试。
#[derive(Debug)]
struct PendingBuffer<T> {
    pending: Mutex<Vec<T>>,
    inflight: Mutex<Vec<T>>,
}

impl<T> Default for PendingBuffer<T> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            inflight: Mutex::new(Vec::new()),
        }
    }
}

impl<T> PendingBuffer<T> {
    fn push(&self, item: T) {
        lock(&self.pending).push(item);
    }

    /// 取出所有「时间戳早于 `cutoff`」的条目。
    ///
    /// 生产者按时间递增顺序 append，且只有刷盘器会移除元素，因此先按谓词过滤、
    /// 再从 `pending` 中移除是安全的，不需要克隆整个缓冲区。
    fn take_before<K, F>(&self, key: F, cutoff: &K, limit: usize) -> Vec<T>
    where
        F: Fn(&T) -> K,
        K: Ord,
    {
        // 上一轮的失败批次仍然优先重试。
        let mut batch = std::mem::take(&mut *lock(&self.inflight));
        let mut pending = lock(&self.pending);
        if batch.len() >= limit {
            return batch;
        }
        let mut kept = Vec::with_capacity(pending.len());
        for item in std::mem::take(&mut *pending) {
            // `key(&item)` 是 `K`，`cutoff` 是 `&K`：用 `Ord::lt(&self, &other)`。
            if batch.len() < limit && key(&item).lt(cutoff) {
                batch.push(item);
            } else {
                kept.push(item);
            }
        }
        *pending = kept;
        batch
    }

    /// 当前尚未落库的条目数（含正在写库的批次）。
    ///
    /// 用于在数据库持续不可用时暴露积压规模：为了不丢数据，这些条目会一直留在
    /// 内存里，因此必须让运维能看到积压而不是静默涨内存。
    fn backlog(&self) -> usize {
        lock(&self.pending).len() + lock(&self.inflight).len()
    }

    /// 写库成功：丢弃 in-flight 批次。重复执行是幂等的，
    /// 即使写库成功但 commit 之前刷盘器被取消，队列也只会被重试而不是丢数据。
    fn commit(&self) {
        lock(&self.inflight).clear();
    }
}

/// 按 `ObjectId` 记录"最新累计大小"的缓冲区，用于 `UPDATE access_*_logs SET body_length`。
///
/// 同一个 id 的多次更新只需保留最新值；刷盘期间到达的新值会覆盖 `pending` 里的旧值，
/// 因此失败回滚时必须用「先到先得」语义，绝不能用旧值覆盖新值。
#[derive(Debug, Default)]
struct SizeBuffer {
    pending: DashMap<ObjectId, usize>,
    inflight: Mutex<Vec<(ObjectId, usize)>>,
}

impl SizeBuffer {
    fn insert(&self, id: ObjectId, size: usize) {
        self.pending.insert(id, size);
    }

    fn take_all(&self, limit: usize) -> Vec<(ObjectId, usize)> {
        let mut batch = std::mem::take(&mut *lock(&self.inflight));
        if batch.len() >= limit {
            return batch;
        }
        let take = limit - batch.len();
        let keys = self
            .pending
            .iter()
            .take(take)
            .map(|e| *e.key())
            .collect::<Vec<_>>();
        for key in keys {
            if let Some((_, value)) = self.pending.remove(&key) {
                batch.push((key, value));
            }
        }
        batch
    }

    fn commit(&self) {
        lock(&self.inflight).clear();
    }

    fn rollback(&self, batch: Vec<(ObjectId, usize)>) {
        for (id, size) in batch {
            // 刷盘期间若已写入更新的值，保留新的那份。
            self.pending.entry(id).or_insert(size);
        }
    }
}

/// 读取锁被 poison 时直接取回内部值：这些缓冲区只是待落库数据的内存队列，
/// 没有需要靠 panic 保护的跨对象不变量，让网关继续工作并保留数据更重要。
fn lock<G>(mutex: &Mutex<G>) -> std::sync::MutexGuard<'_, G> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

static ACCESS_REQUEST_LOGS: LazyLock<PendingBuffer<AccessCreateRequest>> =
    LazyLock::new(PendingBuffer::default);
static ACCESS_RESPONSE_LOGS: LazyLock<PendingBuffer<AccessCreateResponse>> =
    LazyLock::new(PendingBuffer::default);
static ACCESS_REQUEST_SIZE_LOGS: LazyLock<SizeBuffer> = LazyLock::new(SizeBuffer::default);
static ACCESS_RESPONSE_SIZE_LOGS: LazyLock<SizeBuffer> = LazyLock::new(SizeBuffer::default);

static ACCESS_REQUEST_INSERT_SIZE_LOGS: LazyLock<PendingBuffer<AccessInsertRequestSize>> =
    LazyLock::new(PendingBuffer::default);
static ACCESS_RESPONSE_INSERT_SIZE_LOGS: LazyLock<PendingBuffer<AccessInsertResponseSize>> =
    LazyLock::new(PendingBuffer::default);
static CURRENT_TIME: LazyLock<RwLock<Arc<DateTime<Utc>>>> =
    LazyLock::new(|| RwLock::new(Arc::new(Utc::now())));

/// 连续刷盘失败轮数，用于告警（成功一轮即清零）。
static FLUSH_FAILURES: AtomicU64 = AtomicU64::new(0);

pub async fn init_access_logs() -> anyhow::Result<()> {
    tokio::spawn(async move {
        let r = background_update_access_logs().await;
        if let Err(e) = r {
            event!(Level::ERROR, "Failed to update access logs: {}", e);
        }
    });
    Ok(())
}

pub async fn background_update_access_logs() -> anyhow::Result<()> {
    let time = get_database().get_real_database_time().await?;
    let next_time = (time + TimeDelta::seconds(1)).with_nanosecond(0).unwrap();
    // 对齐到下一个整秒
    let offset = (next_time - time).to_std()?;
    let _ = tokio::time::sleep(offset).await;
    loop {
        let cycle_start = Instant::now();
        update_time();
        sync().await;
        let elapsed = cycle_start.elapsed();

        if elapsed > FLUSH_LAG_WARN {
            event!(
                Level::WARN,
                "Access log flush is lagging: one round took {:?} for a 1s window; \
                 the database writes are the bottleneck",
                elapsed
            );
        }

        // 关键修复（P0-4）：间隔下限是 MIN_FLUSH_INTERVAL，而不是 100µs。
        // 当一轮 sync 超过 1s 时直接跳过已错过的整秒，而不是忙等重试，
        // 避免"数据库越慢 → 刷盘越密 → 数据库更慢"的正反馈。
        tokio::time::sleep(MIN_FLUSH_INTERVAL.saturating_sub(elapsed)).await;
    }
}

/// 一轮刷盘。任一子任务失败都不会丢弃内存中的数据，下一轮继续重试。
async fn sync() {
    sync_inner().await;
}

/// 刷盘主体。抽成独立函数是为了能在诊断入口里直接驱动一轮刷盘
/// （`sync()` 只由后台无限循环调用，外部无法复用）。
pub async fn sync_inner() {
    // 先写请求 / 响应主表：`body_length` 的 UPDATE 依赖对应行已经存在。
    let r0 = flush_logged(
        "access request logs",
        &ACCESS_REQUEST_LOGS,
        |b| async move { get_database().insert_batch_access_requests(b).await },
    )
    .await;

    let r1 = flush_logged(
        "access response logs",
        &ACCESS_RESPONSE_LOGS,
        |b| async move { get_database().insert_batch_access_responses(b).await },
    )
    .await;

    // 再并发刷其余四类，缩短单轮耗时。
    let (r2, r3, r4, r5) = tokio::join!(
        flush_size_logs::<AccessUpdateRequestSize, _, _>(
            "access request size updates",
            &ACCESS_REQUEST_SIZE_LOGS,
            |b| async move {
                get_database()
                    .update_batch_access_request_size_logs(b)
                    .await
            },
        ),
        flush_size_logs::<AccessUpdateResponseSize, _, _>(
            "access response size updates",
            &ACCESS_RESPONSE_SIZE_LOGS,
            |b| async move {
                get_database()
                    .update_batch_access_response_size_logs(b)
                    .await
            },
        ),
        flush_logged(
            "access request size inserts",
            &ACCESS_REQUEST_INSERT_SIZE_LOGS,
            |b| async move {
                get_database()
                    .insert_batch_access_request_increase_size_logs(b)
                    .await
            },
        ),
        flush_logged(
            "access response size inserts",
            &ACCESS_RESPONSE_INSERT_SIZE_LOGS,
            |b| async move {
                get_database()
                    .insert_batch_access_response_increase_size_logs(b)
                    .await
            },
        ),
    );

    let failed = [r0, r1, r2, r3, r4, r5]
        .into_iter()
        .filter(|ok| !ok)
        .count();
    if failed == 0 {
        FLUSH_FAILURES.store(0, Ordering::Relaxed);
    } else {
        let rounds = FLUSH_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
        event!(
            Level::ERROR,
            "{failed} of 6 access-log flushes failed (consecutive failing rounds: {rounds}); \
             the batches stay in memory and will be retried, expect a backlog if this persists"
        );
    }

    // 暴露内存积压规模：日志已不再丢弃，代价是 DB 长时间不可用时会占内存。
    let backlog = ACCESS_REQUEST_LOGS.backlog()
        + ACCESS_RESPONSE_LOGS.backlog()
        + ACCESS_REQUEST_INSERT_SIZE_LOGS.backlog()
        + ACCESS_RESPONSE_INSERT_SIZE_LOGS.backlog();
    if backlog >= FLUSH_BACKLOG_WARN {
        event!(
            Level::WARN,
            "Access log backlog in memory: {backlog} entries pending flush;              the database has been unavailable or too slow for a while"
        );
    }
}

/// 按「先写库 → 成功才清理内存 → 失败则回滚重试」刷入一批按时间切分的日志。
///
/// [`PendingBuffer::take_before`] 取出的是已进入 `inflight` 的批次，写入失败时
/// 该批次仍留在 `inflight` 中，下一轮会自动再次取出重试，因此这里无需显式回滚。
async fn flush_logged<T, W, Fut>(
    name: &'static str,
    buffer: &'static LazyLock<PendingBuffer<T>>,
    write: W,
) -> bool
where
    T: HasLogTime + Send + 'static,
    W: FnOnce(Vec<T>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // 取 `DateTime<Utc>` 的所有权作为比较 key（该类型是 Copy，代价极低）。
    let cutoff = { *CURRENT_TIME.read().unwrap().clone() };
    let batch = buffer.take_before(|item: &T| *item.log_time(), &cutoff, MAX_ROWS_PER_FLUSH);
    if batch.is_empty() {
        return true;
    }
    let count = batch.len();
    match write(batch).await {
        Ok(()) => {
            buffer.commit();
            true
        }
        Err(e) => {
            event!(
                Level::ERROR,
                "Failed to flush {name} ({count} rows); keeping them in memory for retry: {e}"
            );
            false
        }
    }
}

/// 条目的时间戳：用于判断该日志是否已经过了它所属的整秒，
/// 可以安全落库（同一秒内到达的日志不能在刷盘中途被切走）。
trait HasLogTime {
    fn log_time(&self) -> &DateTime<Utc>;
}

impl HasLogTime for AccessCreateRequest {
    fn log_time(&self) -> &DateTime<Utc> {
        &self.requested_at
    }
}

impl HasLogTime for AccessCreateResponse {
    fn log_time(&self) -> &DateTime<Utc> {
        &self.responsed_at
    }
}

impl HasLogTime for AccessInsertRequestSize {
    fn log_time(&self) -> &DateTime<Utc> {
        &self.created_at
    }
}

impl HasLogTime for AccessInsertResponseSize {
    fn log_time(&self) -> &DateTime<Utc> {
        &self.created_at
    }
}

/// 刷入「累计大小」更新。失败时把批次合并回 `pending` 重试，
/// 且不会覆盖刷盘期间到达的更新值。
async fn flush_size_logs<U, W, Fut>(
    name: &'static str,
    buffer: &'static LazyLock<SizeBuffer>,
    write: W,
) -> bool
where
    U: From<(ObjectId, usize)> + Send + 'static,
    W: FnOnce(Vec<U>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let batch = buffer.take_all(MAX_ROWS_PER_FLUSH);
    if batch.is_empty() {
        return true;
    }
    let count = batch.len();
    let updates = batch
        .iter()
        .map(|(id, size)| U::from((*id, *size)))
        .collect();
    match write(updates).await {
        Ok(()) => {
            buffer.commit();
            true
        }
        Err(e) => {
            event!(
                Level::ERROR,
                "Failed to flush {name} ({count} rows); keeping them in memory for retry: {e}"
            );
            buffer.rollback(batch);
            false
        }
    }
}

// ==================== 生产者接口 ====================
//
// 这些函数运行在请求处理热路径上，只做内存写入，绝不阻塞、绝不 panic。

/// 记录请求日志。`RequestLog` 内的时间戳取自数据库时间，
/// 因此 `CURRENT_TIME` 与它同源，可以正确按整秒切分。
pub fn add_request_log(log: &RequestLog) {
    ACCESS_REQUEST_LOGS.push(log.inner.clone());
}

pub fn add_response_log(log: &ResponseLog) {
    ACCESS_RESPONSE_LOGS.push(log.inner.clone());
}

pub fn update_request_size_log(id: ObjectId, size: usize) {
    ACCESS_REQUEST_SIZE_LOGS.insert(id, size);
}

pub fn update_response_size_log(id: ObjectId, size: usize) {
    ACCESS_RESPONSE_SIZE_LOGS.insert(id, size);
}

pub fn insert_increase_request_size_log(id: ObjectId, size: usize) {
    let current_time = { *CURRENT_TIME.read().unwrap().clone() };
    ACCESS_REQUEST_INSERT_SIZE_LOGS.push(AccessInsertRequestSize::new(id, size, current_time));
}

pub fn insert_increase_response_size_log(id: ObjectId, size: usize) {
    let current_time = { *CURRENT_TIME.read().unwrap().clone() };
    ACCESS_RESPONSE_INSERT_SIZE_LOGS.push(AccessInsertResponseSize::new(id, size, current_time));
}

/// 把 `CURRENT_TIME` 推进到当前数据库时间的整秒。
fn update_time() {
    let current_time = match get_database().get_database_time() {
        Ok(t) => t,
        Err(e) => {
            event!(
                Level::WARN,
                "Failed to read database time, using local time: {e}"
            );
            Utc::now()
        }
    }
    .with_nanosecond(0)
    .unwrap_or_else(Utc::now);
    let mut time = CURRENT_TIME.write().unwrap_or_else(|e| e.into_inner());
    *time = Arc::new(current_time);
}
