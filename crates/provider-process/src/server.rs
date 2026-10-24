use crate::protocol::*;
use serde_json::Value;
use std::{
    io::{self, BufReader},
    sync::{Arc, Mutex},
};
use tradeslope_sdk::runtime::{Database, ProviderSession};
use tradeslope_sdk::{Backend, BackendHost, BackendResult, Permissions};

/// Run one trusted backend on stdin/stdout. Do not log to stdout.
/// No BackendRuntime is created: this session must not alter persisted enablement.
pub fn serve(backend: Arc<dyn Backend>) -> BackendResult<()> {
    // Host supplies strict nested parameter and output validation, but does not
    // start the backend. The parent application enforces actual access grants.
    let host = BackendHost::new(vec![backend.clone()])?;
    let client = host.client(Permissions {
        private_read: true,
        trade: true,
    });
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let mut input = BufReader::new(io::stdin());
    let mut session: Option<ProviderSession> = None;
    let mut metadata_sent = false;
    let mut last_id = 0;
    let run = (|| {
        while let Some(request) = read::<Request>(&mut input, MAX_REQUEST_BYTES)? {
            if request.version != VERSION || request.id <= last_id {
                return Err(failure("invalid_request"));
            }
            last_id = request.id;
            let shutdown = matches!(request.method, Method::Shutdown);
            let result = (|| -> BackendResult<Value> {
                match request.method {
                    Method::Metadata => {
                        if metadata_sent || session.is_some() {
                            return Err(failure("already_initialized"));
                        }
                        let metadata = serde_json::to_value(Metadata {
                            manifest: backend.manifest(),
                            sources: backend.sources(),
                            settings: backend.settings(),
                        })
                        .map_err(|_| failure("invalid_metadata"))?;
                        metadata_sent = true;
                        Ok(metadata)
                    }
                    Method::Initialize { db_path, context } => {
                        if !metadata_sent || session.is_some() {
                            return Err(failure("invalid_initialization"));
                        }
                        let output = stdout.clone();
                        let created = ProviderSession::new(
                            backend.clone(),
                            Database::open(db_path)?,
                            context.settings,
                            context.accounts,
                            context.selected,
                            Some(Arc::new(move |_| {
                                if let Ok(mut output) = output.lock() {
                                    let _ = write(
                                        &mut *output,
                                        &Frame::Changed { version: VERSION },
                                        MAX_RESPONSE_BYTES,
                                    );
                                }
                            })),
                        )?;
                        session = Some(created);
                        backend.start(session.as_ref().unwrap().services())?;
                        Ok(Value::Null)
                    }
                    Method::Context(context) => {
                        session
                            .as_ref()
                            .ok_or_else(|| failure("not_initialized"))?
                            .update(context.settings, context.accounts, context.selected)?;
                        Ok(Value::Null)
                    }
                    Method::Execute { operation, params } => {
                        if session.is_none() {
                            return Err(failure("not_initialized"));
                        }
                        if encode(&params, tradeslope_sdk::MAX_PARAMS_BYTES + 1).is_err() {
                            return Err(failure("params_too_large"));
                        }
                        client.call(&backend.manifest().id, &operation, params)
                    }
                    Method::Shutdown => Ok(Value::Null),
                }
            })();
            let frame = Frame::Response {
                version: VERSION,
                id: request.id,
                result: result.map_err(|_| failure("provider_error")),
            };
            write(
                &mut *stdout.lock().map_err(|_| failure("process_io"))?,
                &frame,
                MAX_RESPONSE_BYTES,
            )?;
            if shutdown {
                break;
            }
        }
        Ok(())
    })();
    if let Some(session) = session {
        session.cancel();
        backend.stop()?;
    }
    run
}
