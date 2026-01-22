use dashmap::{DashMap, Entry};
use http::{Request, Response};
use http_body_util::BodyExt;
use std::sync::{Arc, Mutex};

use crate::{
    AsyncRuntime, NormalizedBody, NormalizedData,
    application::{ApplicationPlugin, Session},
    service::Service,
    transport::TransportPlugin,
};

/// The core internal service that performs network I/O with Session Pooling.
pub struct BaseService<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
{
    transport: T,
    app: A,
    runtime: R,
    /// Thread-safe pool of sessions keyed by URI authority.
    pool: Arc<DashMap<String, Mutex<A::Session>>>,
}

impl<T, A, R> BaseService<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
{
    pub fn new(transport: T, app: A, runtime: R) -> Self {
        Self {
            transport,
            app,
            runtime,
            pool: Default::default(),
        }
    }
}

impl<T, A, R> Service for BaseService<T, A, R>
where
    T: TransportPlugin,
    A: ApplicationPlugin<T::Conn>,
    R: AsyncRuntime,
{
    async fn handle(
        &self,
        req: Request<NormalizedBody>,
    ) -> anyhow::Result<Response<NormalizedBody>> {
        // Extract the authority (host:port) to use as the cache key
        let authority = req
            .uri()
            .authority()
            .map(|a| a.to_string())
            .ok_or_else(|| anyhow::anyhow!("URI missing authority"))?;

        // 1. Attempt to find an existing session in the DashMap
        let mut session = match self.pool.entry(authority) {
            Entry::Occupied(occupied) => occupied.get().lock().unwrap().clone(),
            Entry::Vacant(vacant) => {
                // 2. If no session exists, establish a new transport connection
                let conn = self.transport.connect(req.uri()).await?;

                // 3. Perform the application-layer handshake
                let (new_session, driver) = self.app.handshake(conn).await?;

                // 4. Drive the protocol in the background
                self.runtime.spawn(driver);

                // 5. Store the session in the pool.
                vacant.insert(Mutex::new(new_session.clone()));
                new_session
            }
        };

        // 6. Send the request through the session
        let (res_parts, res_body) = session.send_request(req).await?.into_parts();

        // 7. Normalize the response body
        let res_body = res_body
            .map_frame(|frame| frame.map_data(|data| Box::new(data) as NormalizedData))
            .map_err(|err| err.into())
            .boxed();

        Ok(Response::from_parts(res_parts, res_body))
    }
}
