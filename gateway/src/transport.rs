use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

use anyhow::Error;
use bytes::Bytes;
use http_body::Frame;
use http_body_util::Full;
use hyper::{
    Response,
    body::{Body, Incoming},
};
use shared::objectid::ObjectId;

use crate::access::{
    insert_increase_request_size_log, insert_increase_response_size_log, update_request_size_log,
    update_response_size_log,
};

// 创建一个统一的 body 类型
#[derive(Debug)]
pub enum CResponse {
    Incoming(StatisticsIncoming),
    Error(Full<Bytes>),
}

#[derive(Debug)]
pub enum StatisticsIncomingType {
    Request,
    Response,
}

#[derive(Debug)]
pub struct StatisticsIncoming {
    pub inner: Incoming,
    id: ObjectId,
    method: StatisticsIncomingType,
    total_size: usize,
    size: usize,
    /// 响应体是否已被完整读到 EOF。
    ///
    /// 由连接池的归还逻辑读取：只有观察到 EOF 才允许把上游连接放回池中复用。
    /// 客户端中途断开、响应体被丢弃时该标志保持 `false`，连接会被关闭，
    /// 避免残留的响应字节被下一个请求读到（ISSUES.md P0-5）。
    body_drained: Option<Arc<AtomicBool>>,
}

impl StatisticsIncoming {
    pub fn new(id: ObjectId, inner: Incoming, method: StatisticsIncomingType) -> Self {
        Self {
            inner,
            id,
            method,
            size: 0,
            total_size: 0,
            body_drained: None,
        }
    }

    /// 附加一个"响应体已读到 EOF"的信号量，供连接池判断连接能否安全复用。
    pub fn with_body_drained_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.body_drained = Some(flag);
        self
    }

    fn mark_drained(&self) {
        if let Some(flag) = &self.body_drained {
            flag.store(true, Ordering::Release);
        }
    }

    pub fn real_size_hint(&self) -> usize {
        self.total_size
    }

    fn increase_size(&mut self) {
        let current_size = self.size;
        match self.method {
            StatisticsIncomingType::Request => {
                insert_increase_request_size_log(self.id, current_size);
            }
            StatisticsIncomingType::Response => {
                insert_increase_response_size_log(self.id, current_size)
            }
        }
        self.size -= current_size;
    }

    fn update_size(&self) {
        match self.method {
            StatisticsIncomingType::Request => {
                update_request_size_log(self.id, self.total_size);
            }
            StatisticsIncomingType::Response => update_response_size_log(self.id, self.total_size),
        }
    }
}

impl Drop for StatisticsIncoming {
    fn drop(&mut self) {
        self.update_size();
    }
}

impl http_body::Body for StatisticsIncoming {
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    /// Attempt to pull out the next data buffer of this stream.
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, <CResponse as Body>::Error>>> {
        let res = Pin::new(&mut self.inner).poll_frame(cx).map(|opt| {
            if opt.is_none() {
                // 读到流结束：响应体已完整消费，连接可以安全复用。
                self.mark_drained();
            }
            opt.map(|result| {
                result.map_err(|e| anyhow::anyhow!(e).into()).map(|v| {
                    v.map_data(|data| {
                        self.total_size += data.len();
                        self.size += data.len();
                        self.increase_size();
                        data
                    })
                })
            })
        });

        // is end stream
        if self.inner.is_end_stream() {
            self.mark_drained();
            self.update_size();
        }

        res
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl From<String> for CResponse {
    fn from(s: String) -> Self {
        CResponse::Error(Full::new(Bytes::from(s)))
    }
}

impl CResponse {
    pub fn new_from_string(value: impl Into<String>) -> Self {
        CResponse::from(value.into())
    }
}

impl http_body::Body for CResponse {
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    /// Attempt to pull out the next data buffer of this stream.
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, <CResponse as Body>::Error>>> {
        match &mut *self {
            Self::Incoming(incoming) => Pin::new(incoming)
                .poll_frame(cx)
                .map(|opt| opt.map(|result| result.map_err(|e| anyhow::anyhow!(e).into()))),
            Self::Error(full) => Pin::new(full)
                .poll_frame(cx)
                .map(|opt| opt.map(|result| result.map_err(|_| anyhow::anyhow!("error").into()))),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Incoming(incoming) => incoming.is_end_stream(),
            Self::Error(full) => full.is_end_stream(),
        }
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match self {
            Self::Incoming(incoming) => incoming.size_hint(),
            Self::Error(full) => full.size_hint(),
        }
    }
}

pub enum CResponseResult {
    Backend(Response<CResponse>),
    NotFoundGateway,
    GatewayError(Error),
    BadRequest,
    Timeout,
}

pub enum CFirstResponse {
    Error,
    Response(CResponseResult),
}
