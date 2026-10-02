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

/// 按 `(ObjectId, 秒)` **聚合**"响应体大小增量"，每轮刷盘每个 (id, 秒) 只写一行。
///
/// 为什么需要它：`StatisticsIncoming` 每读到一个 body chunk 就会调用一次
/// `insert_increase_response_size_log`（见 `transport.rs` 的 `increase_size`），
/// 而原先的实现会为每次调用**新生成一个 ObjectId 并插一行**。于是一个响应体被分 N 块
/// 读取就写 N 行 —— 生产库实测 `access_response_size_logs` 有 **1600 万行 / 3.6 GB**，
/// 是请求主表的 **7 倍**（单条 `response_id` 最多挂 262791 行）。
///
/// **为什么键里带"秒"而不是只带 id**：这两张表的用途就是**按秒统计请求/响应体大小**
/// （需求原文："可以细化到每秒的颗粒度"）。只按 id 聚合成一行会把秒级颗粒度彻底丢掉；
/// 按 `(id, 秒)` 聚合则：同一秒内的多个 chunk 合并成一行，跨秒仍然分开 —— 粒度不丢，
/// 行数按"请求数 × 实际跨秒数"收敛（单秒内完成的请求只占一行）。
///
/// 落库侧还有 `UNIQUE (xxx_id, at_second)` + `ON CONFLICT DO UPDATE body_length =
/// body_length + EXCLUDED.body_length` 兜底，因此即使同一秒的数据分两轮刷盘也会累加。
///
/// `created_at` 取该 (id, 秒) **首次出现**的时间戳；`at_second` 为截断到整秒的键。
#[derive(Debug, Default)]
struct SizeAccumulator {
    /// key = (id, 截断到整秒的时间)
    pending: DashMap<(ObjectId, DateTime<Utc>), AccessInsertResponseSize>,
    inflight: Mutex<Vec<AccessInsertResponseSize>>,
}

impl SizeAccumulator {
    fn add(&self, id: ObjectId, size: usize, at: DateTime<Utc>) {
        // 键用截断到整秒的时间，与落库侧的 UNIQUE (xxx_id, at_second) 对齐。
        // 必须截断：`DateTime<Utc>` 的小数部分会让同一秒内的 key 互不相等。
        // 用时间戳算术而不是 `chrono::Rounding::trunc_subsecs`，省一个 trait 导入。
        let at_second = DateTime::from_timestamp(at.timestamp(), 0).expect("valid timestamp");
        let key = (id, at_second);
        self.pending
            .entry(key)
            .and_modify(|v| v.body_length += size)
            .or_insert_with(|| AccessInsertResponseSize::new(id, size, at));
    }

    fn take_all(&self, limit: usize) -> Vec<AccessInsertResponseSize> {
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
                batch.push(value);
            }
        }
        batch
    }

    fn commit(&self) {
        lock(&self.inflight).clear();
    }

    fn rollback(&self, batch: Vec<AccessInsertResponseSize>) {
        for item in batch {
            // 刷盘期间若同一 (id, 秒) 又累加了新字节，必须**相加**而不是覆盖。
            let key = (item.id, item.at_second);
            self.pending
                .entry(key)
                .and_modify(|v| v.body_length += item.body_length)
                .or_insert(item);
        }
    }

    fn backlog(&self) -> usize {
        self.pending.len() + lock(&self.inflight).len()
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
static ACCESS_RESPONSE_INSERT_SIZE_LOGS: LazyLock<SizeAccumulator> =
    LazyLock::new(SizeAccumulator::default);
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
        flush_accumulated_size_logs(
            "access response size inserts",
            &ACCESS_RESPONSE_INSERT_SIZE_LOGS,
            |b: Vec<AccessInsertResponseSize>| async move {
                // 交还批次：失败时 `rollback` 需要它把累加值放回内存。
                let r = get_database()
                    .insert_batch_access_response_increase_size_logs(b.clone())
                    .await;
                (b, r)
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

/// 刷入「按 id 聚合后的响应大小」。与 [`flush_size_logs`] 的区别是：
/// 它面对的是 `SizeAccumulator`（累加器），失败时要把批次**相加**回 `pending`
/// 而不是覆盖，否则刷盘期间新到的字节会被丢掉。
async fn flush_accumulated_size_logs<W, Fut>(
    name: &'static str,
    buffer: &'static LazyLock<SizeAccumulator>,
    write: W,
) -> bool
where
    W: FnOnce(Vec<AccessInsertResponseSize>) -> Fut,
    Fut: Future<Output = (Vec<AccessInsertResponseSize>, anyhow::Result<()>)>,
{
    let batch = buffer.take_all(MAX_ROWS_PER_FLUSH);
    if batch.is_empty() {
        return true;
    }
    let count = batch.len();
    // 闭包把批次**交还**回来（成功与失败都交还），失败时据此回滚。
    // 不能像 `flush_logged` 那样直接 `write(batch)` —— 那样批次被消费掉，
    // 失败后无法把累加值放回内存，会静默丢失这批字节统计。
    let (returned, result) = write(batch).await;
    match result {
        Ok(()) => {
            buffer.commit();
            true
        }
        Err(e) => {
            event!(
                Level::ERROR,
                "Failed to flush {name} ({count} rows); keeping them in memory for retry: {e}"
            );
            buffer.rollback(returned);
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

/// 与 [`insert_increase_response_size_log`] 相同，但显式指定时间戳（测试用）。
#[cfg(test)]
fn insert_increase_response_size_log_at(id: ObjectId, size: usize, at: DateTime<Utc>) {
    ACCESS_RESPONSE_INSERT_SIZE_LOGS.add(id, size, at);
}

pub fn insert_increase_response_size_log(id: ObjectId, size: usize) {
    // 聚合到"每个响应一行"，而不是每个 body chunk 一行（见 `SizeAccumulator` 说明）。
    let current_time = { *CURRENT_TIME.read().unwrap().clone() };
    ACCESS_RESPONSE_INSERT_SIZE_LOGS.add(id, size, current_time);
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

// ==================== 历史日志清理（保留期） ====================

/// 清理任务的轮询间隔。保留期以"天"为单位，因此不需要频繁检查；
/// 真正的推进速度由 `shared::database::access::prune_access_logs` 的
/// 分批删除控制（每轮 5 万行）。
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// 启动历史访问日志清理任务。
///
/// 放在数据面（gateway）而不是控制面：这张表由 gateway 写入，清理也应当由
/// 数据面负责；同时 `prune_access_logs` 内部用 `pg_try_advisory_lock` 保证
/// 多实例并发时只有一个在执行，因此多副本部署是安全的。
pub async fn init_access_log_pruner() {
    tokio::spawn(async move {
        loop {
            match shared::database::access::prune_access_logs().await {
                Ok(0) => {}
                Ok(n) => event!(Level::INFO, "Pruned {n} historical access log rows"),
                Err(e) => event!(Level::ERROR, "Failed to prune access logs: {e}"),
            }
            tokio::time::sleep(PRUNE_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归测试（生产事故）：同一秒内的多个 body chunk 必须**累积成一行**，
    /// 但**跨秒**仍要分开 —— 这两张表的用途是按秒统计大小，秒级颗粒度不能丢。
    ///
    /// 历史行为是「每个 chunk 生成一个新 ObjectId 并插一行」，导致生产库
    /// `access_response_size_logs` 涨到 1600 万行 / 3.6 GB（请求主表的 7 倍）。
    #[test]
    fn response_size_chunks_accumulate_per_second() {
        let id = ObjectId::new();
        let base = DateTime::from_timestamp(1_800_000_000, 0).expect("valid timestamp");

        // 同一秒内的三块
        insert_increase_response_size_log_at(id, 10, base);
        insert_increase_response_size_log_at(id, 25, base + TimeDelta::milliseconds(300));
        insert_increase_response_size_log_at(id, 65, base + TimeDelta::milliseconds(900));
        // 下一秒的一块
        insert_increase_response_size_log_at(id, 7, base + TimeDelta::seconds(1));

        let mut batch = ACCESS_RESPONSE_INSERT_SIZE_LOGS.take_all(100);
        batch.sort_by_key(|v| v.at_second);
        assert_eq!(batch.len(), 2, "同一个 id 每秒只应有一行（共 2 秒 → 2 行）");
        assert_eq!(batch[0].at_second, base);
        assert_eq!(
            batch[0].body_length,
            10 + 25 + 65,
            "同一秒内的 chunk 必须累加"
        );
        assert_eq!(batch[1].at_second, base + TimeDelta::seconds(1));
        assert_eq!(batch[1].body_length, 7, "不同秒的行必须分开，保留秒级颗粒度");

        ACCESS_RESPONSE_INSERT_SIZE_LOGS.commit();
    }

    /// 回滚必须是**相加**而不是覆盖：刷盘期间同一 (id, 秒) 又到了新字节时，
    /// 用旧值覆盖会把新字节丢掉。
    #[test]
    fn accumulator_rollback_adds_instead_of_overwriting() {
        let id = ObjectId::new();
        let base = DateTime::from_timestamp(1_800_000_000, 0).expect("valid timestamp");
        let acc = SizeAccumulator::default();

        acc.add(id, 100, base);
        let taken = acc.take_all(10);
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].body_length, 100);

        // 刷盘进行中，同一秒又有 40 字节到达。
        acc.add(id, 40, base + TimeDelta::milliseconds(500));
        // 写库失败 → 回滚。
        acc.rollback(taken);

        let again = acc.take_all(10);
        assert_eq!(again.len(), 1, "同一秒必须仍然只有一行");
        assert_eq!(
            again[0].body_length,
            140,
            "回滚必须与刷盘期间新到的字节相加（100 + 40），不能覆盖成 100"
        );

        // 不同秒不会被合并
        acc.add(id, 5, base + TimeDelta::seconds(3));
        let third = acc.take_all(10);
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].at_second, base + TimeDelta::seconds(3));
    }
}
